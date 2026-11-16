#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

mod frag;
mod im;
mod logs;
mod privacy;
mod recycle;
mod rules;
mod safety;
mod shred;
mod timeutil;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

use std::os::windows::process::CommandExt;

const RULES_JSON: &str = include_str!("../../rules.json");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

#[derive(Serialize, Clone)]
struct ScanStatus {
    active: bool,
    stage: String,
    cancelled: bool,
}

static SCAN_STATUS: OnceLock<Mutex<HashMap<String, ScanStatus>>> = OnceLock::new();

fn scan_status_cell() -> &'static Mutex<HashMap<String, ScanStatus>> {
    SCAN_STATUS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn scan_begin(kind: &str, stage: &str) {
    if let Ok(mut map) = scan_status_cell().lock() {
        map.insert(
            kind.to_string(),
            ScanStatus {
                active: true,
                stage: stage.to_string(),
                cancelled: false,
            },
        );
    }
}

fn scan_end(kind: &str) {
    if let Ok(mut map) = scan_status_cell().lock() {
        if let Some(status) = map.get_mut(kind) {
            status.active = false;
        }
    }
}

fn scan_was_cancelled(kind: &str) -> bool {
    scan_status_cell()
        .lock()
        .ok()
        .and_then(|map| map.get(kind).map(|status| status.cancelled))
        .unwrap_or(false)
}

#[tauri::command]
fn scan_status() -> HashMap<String, ScanStatus> {
    scan_status_cell()
        .lock()
        .map(|map| map.clone())
        .unwrap_or_default()
}

/// 不传 kind 时取消所有正在跑的扫描。
#[tauri::command]
fn cancel_scan(kind: Option<String>) {
    if let Ok(mut map) = scan_status_cell().lock() {
        match kind {
            Some(kind) => {
                if let Some(status) = map.get_mut(&kind) {
                    if status.active {
                        status.cancelled = true;
                    }
                }
            }
            None => {
                for status in map.values_mut() {
                    if status.active {
                        status.cancelled = true;
                    }
                }
            }
        }
    }
}

const TOP_SKIP: [&str; 13] = [
    "$recycle.bin",
    "system volume information",
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "recovery",
    "perflogs",
    "msocache",
    "config.msi",
    "$windows.~bt",
    "$windows.~ws",
    "windows.old",
];

// ---------- 数据结构 ----------

#[derive(Serialize)]
struct DiskInfo {
    total: u64,
    used: u64,
    free: u64,
}

#[derive(Serialize, Clone)]
struct CategorySize {
    key: String,
    name: String,
    tier: String,
    describe: String,
    warning: String,
    note: String,
    size: i64,
    count: u64,
    requires_confirm: bool,
    implemented: bool,
}

#[derive(Serialize)]
struct BigFile {
    path: String,
    size: u64,
    mtime: u64,
}

#[derive(Serialize, Deserialize, Clone)]
struct DupGroup {
    hash: String,
    size: u64,
    files: Vec<String>,
    mtimes: Vec<u64>,
}

#[derive(Deserialize)]
struct RulesFile {
    #[serde(default)]
    categories: Vec<Rule>,
}

#[derive(Deserialize, Clone)]
struct Rule {
    #[serde(default)]
    key: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    tier: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    describe: String,
    #[serde(default)]
    warning: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    glob_dirs: Vec<String>,
    #[serde(default)]
    patterns: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    min_age_minutes: u64,
    #[serde(default)]
    requires_confirm: bool,
    #[serde(default)]
    prune_dirs: bool,
}

// ---------- 命令：磁盘信息 ----------

#[tauri::command]
fn disk_info() -> Result<DiskInfo, String> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let root: Vec<u16> = "C:\\".encode_utf16().chain(std::iter::once(0)).collect();
    let mut free_to_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 || total == 0 {
        return Err("读取磁盘信息失败".into());
    }
    Ok(DiskInfo {
        total,
        used: total.saturating_sub(total_free),
        free: total_free,
    })
}

// ---------- 命令：清理项统计 ----------

#[tauri::command]
fn scan_categories() -> Vec<CategorySize> {
    let rules = all_rules();
    let mut out: Vec<CategorySize> = rules.par_iter().filter_map(scan_rule).collect();
    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}

/// 内置规则 + 远程更新下来的规则（按 key 去重）。
pub(crate) fn all_rules() -> Vec<Rule> {
    let mut rules: Vec<Rule> = serde_json::from_str::<RulesFile>(RULES_JSON)
        .map(|data| data.categories)
        .unwrap_or_default();
    if let Ok(text) = std::fs::read_to_string(rules::remote_rules_path()) {
        if let Ok(extra) = serde_json::from_str::<RulesFile>(&text) {
            let mut known: HashSet<String> = rules.iter().map(|rule| rule.key.clone()).collect();
            for rule in extra.categories {
                if !rule.key.is_empty() && known.insert(rule.key.clone()) {
                    rules.push(rule);
                }
            }
        }
    }
    rules
}

fn scan_rule(rule: &Rule) -> Option<CategorySize> {
    let kind: &str = if rule.kind.is_empty() {
        "files"
    } else {
        rule.kind.as_str()
    };
    if rule.key.is_empty() {
        return None;
    }
    if kind != "files" {
        return Some(CategorySize {
            key: rule.key.clone(),
            name: if rule.name.is_empty() {
                rule.key.clone()
            } else {
                rule.name.clone()
            },
            tier: if rule.tier.is_empty() {
                "standard".into()
            } else {
                rule.tier.clone()
            },
            describe: rule.describe.clone(),
            warning: rule.warning.clone(),
            note: rule.note.clone(),
            size: 0,
            count: 0,
            requires_confirm: true,
            implemented: false,
        });
    }
    let mut roots = expand_paths(&rule.paths);
    roots.extend(
        expand_paths(&rule.glob_dirs)
            .into_iter()
            .filter(|p| p.is_dir()),
    );
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        return None;
    }
    let include = build_matcher(&rule.patterns);
    let excludes: Vec<String> = rule.exclude.iter().map(|p| normalize_exclude(p)).collect();
    let exclude = build_matcher(&excludes);
    let min_age = Duration::from_secs(rule.min_age_minutes.saturating_mul(60));
    let now = SystemTime::now();

    let mut size: u64 = 0;
    let mut count: u64 = 0;
    for root in &roots {
        if root.is_file() {
            if let Ok(meta) = root.symlink_metadata() {
                if !is_reparse(&meta) {
                    let name = root
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if include_matches(&name, &include, &exclude) && age_ok(&meta, min_age, now) {
                        size += meta.len();
                        count += 1;
                    }
                }
            }
            continue;
        }
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            // walkdir 在 Windows 上直接用目录枚举里的属性，不必再 stat 一次。
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if is_reparse(&meta) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !include_matches(&name, &include, &exclude) || !age_ok(&meta, min_age, now) {
                continue;
            }
            size += meta.len();
            count += 1;
        }
    }

    Some(CategorySize {
        key: rule.key.clone(),
        name: if rule.name.is_empty() {
            rule.key.clone()
        } else {
            rule.name.clone()
        },
        tier: if rule.tier.is_empty() {
            "standard".into()
        } else {
            rule.tier.clone()
        },
        describe: rule.describe.clone(),
        warning: rule.warning.clone(),
        note: rule.note.clone(),
        size: size as i64,
        count,
        requires_confirm: rule.requires_confirm || rule.tier == "deep",
        implemented: true,
    })
}

fn normalize_exclude(pattern: &str) -> String {
    // 规则里的 ".pdf" 这类裸扩展名按文件名后缀处理。
    if let Some(rest) = pattern.strip_prefix('.') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric()) {
            return format!("*{}", pattern);
        }
    }
    pattern.to_string()
}

fn build_matcher(patterns: &[String]) -> Vec<glob::Pattern> {
    let source: Vec<String> = if patterns.is_empty() {
        vec!["*".to_string()]
    } else {
        patterns.to_vec()
    };
    source
        .iter()
        .filter_map(|p| glob::Pattern::new(&p.to_lowercase()).ok())
        .collect()
}

fn include_matches(name: &str, include: &[glob::Pattern], exclude: &[glob::Pattern]) -> bool {
    include.iter().any(|p| p.matches(name)) && !exclude.iter().any(|p| p.matches(name))
}

fn age_ok(meta: &std::fs::Metadata, min_age: Duration, now: SystemTime) -> bool {
    if min_age.is_zero() {
        return true;
    }
    let Ok(modified) = meta.modified() else {
        return true;
    };
    now.duration_since(modified)
        .map(|d| d >= min_age)
        .unwrap_or(true)
}

pub(crate) fn expand_env(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' {
            if let Some(offset) = chars[i + 1..].iter().position(|&c| c == '%') {
                let name: String = chars[i + 1..i + 1 + offset].iter().collect();
                if let Ok(value) = std::env::var(&name) {
                    out.push_str(&value);
                    i += offset + 2;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn expand_paths(raw: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in raw {
        let expanded = expand_env(item);
        if expanded.contains('*') || expanded.contains('?') {
            if let Ok(matches) = glob::glob(&expanded) {
                for path in matches.flatten() {
                    if path.exists() {
                        let key = path.to_string_lossy().to_lowercase();
                        if seen.insert(key) {
                            out.push(path);
                        }
                    }
                }
            }
        } else {
            let path = PathBuf::from(&expanded);
            if path.exists() {
                let key = path.to_string_lossy().to_lowercase();
                if seen.insert(key) {
                    out.push(path);
                }
            }
        }
    }
    out
}

fn is_reparse(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

// ---------- 命令：大文件 ----------

#[tauri::command]
fn scan_big(drives: Option<Vec<String>>) -> Vec<BigFile> {
    scan_begin("big", "大文件扫描");
    let list = drives.unwrap_or_else(fixed_drives);
    let mut rows: Vec<BigFile> = list
        .par_iter()
        .flat_map(|drive| {
            let files = if scan_was_cancelled("big") {
                Vec::new()
            } else {
                mft_files(drive, 100 * 1024 * 1024, &TOP_SKIP)
            };
            files
                .into_par_iter()
                .map(|(path, size)| BigFile {
                    mtime: file_mtime(&path),
                    path,
                    size,
                })
                .collect::<Vec<_>>()
        })
        .filter(|row| !is_system_file_name(&row.path))
        .collect();
    rows.sort_by(|a, b| b.size.cmp(&a.size));
    scan_end("big");
    rows
}

fn is_system_file_name(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    matches!(
        name.as_str(),
        "pagefile.sys" | "hiberfil.sys" | "swapfile.sys" | "dumpstack.log.tmp" | "memory.dmp"
    )
}

// ---------- 命令：重复文件 ----------

#[tauri::command]
fn scan_dups(root: Option<String>, min_size: Option<u64>) -> Vec<DupGroup> {
    scan_begin("dup", "重复文件扫描");
    let trace = std::env::var("CDC_TRACE").is_ok();
    let roots = root.map(|r| vec![r]).unwrap_or_else(fixed_drives);
    let min = min_size.unwrap_or(1 << 20);
    let cache_key = blake3::hash(format!("{:?}:{}", roots, min).as_bytes())
        .to_hex()
        .to_string();
    let cache_path = cache_dir().join(format!("dups-{}.json", cache_key));
    if let Ok(meta) = std::fs::metadata(&cache_path) {
        let fresh = meta
            .modified()
            .ok()
            .and_then(|time| time.elapsed().ok())
            .map(|age| age < Duration::from_secs(300))
            .unwrap_or(false);
        if fresh {
            if let Ok(text) = std::fs::read_to_string(&cache_path) {
                if let Ok(groups) = serde_json::from_str::<Vec<DupGroup>>(&text) {
                    scan_end("dup");
                    return groups;
                }
            }
        }
    }
    let started = std::time::Instant::now();
    let mut by_size: HashMap<u64, Vec<String>> = HashMap::new();
    for drive in &roots {
        if scan_was_cancelled("dup") {
            break;
        }
        for (path, size) in mft_files(drive, min, &TOP_SKIP) {
            by_size.entry(size).or_default().push(path);
        }
    }
    let candidates: usize = by_size
        .values()
        .filter(|paths| paths.len() >= 2)
        .map(|paths| paths.len())
        .sum();
    if trace {
        eprintln!(
            "dup: 枚举 {} 个尺寸组, 参与比对 {} 个文件, {:.1}s",
            by_size.len(),
            candidates,
            started.elapsed().as_secs_f64()
        );
    }

    let mut groups: Vec<DupGroup> = by_size
        .par_iter()
        .filter(|(_, paths)| paths.len() >= 2 && !scan_was_cancelled("dup"))
        .flat_map(|(size, paths)| {
            if scan_was_cancelled("dup") {
                return Vec::new();
            }
            let sampled: Vec<(String, Option<String>)> = paths
                .par_iter()
                .map(|path| (path.clone(), sample_hash(Path::new(path), *size)))
                .collect();
            let mut by_sample: HashMap<String, Vec<String>> = HashMap::new();
            for (path, digest) in sampled {
                if let Some(digest) = digest {
                    by_sample.entry(digest).or_default().push(path);
                }
            }
            let mut found = Vec::new();
            for candidates in by_sample.into_values().filter(|c| c.len() >= 2) {
                if scan_was_cancelled("dup") {
                    break;
                }
                let hashed: Vec<(String, Option<String>)> = candidates
                    .par_iter()
                    .map(|path| (path.clone(), full_hash(Path::new(path))))
                    .collect();
                let mut by_hash: HashMap<String, Vec<String>> = HashMap::new();
                for (path, digest) in hashed {
                    if let Some(digest) = digest {
                        by_hash.entry(digest).or_default().push(path);
                    }
                }
                for (hash, mut files) in by_hash {
                    if files.len() < 2 {
                        continue;
                    }
                    files.sort();
                    let mtimes = files.iter().map(|p| file_mtime(p)).collect();
                    found.push(DupGroup {
                        hash,
                        size: *size,
                        files,
                        mtimes,
                    });
                }
            }
            found
        })
        .collect();

    groups.sort_by(|a, b| {
        let left = a.size * a.files.len() as u64;
        let right = b.size * b.files.len() as u64;
        right.cmp(&left)
    });
    if trace {
        eprintln!(
            "dup: 找到 {} 组, 总用时 {:.1}s",
            groups.len(),
            started.elapsed().as_secs_f64()
        );
    }
    if !scan_was_cancelled("dup") && !groups.is_empty() {
        if let Ok(text) = serde_json::to_string(&groups) {
            let _ = std::fs::write(&cache_path, text);
        }
    }
    scan_end("dup");
    groups
}

/// 删除文件后清掉重复文件缓存，避免 5 分钟内再次扫描还显示已删掉的组。
pub(crate) fn invalidate_dup_cache() {
    let dir = cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("dups-") && name.ends_with(".json") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn sample_hash(path: &Path, size: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; 8192];
    let mut digest = blake3::Hasher::new();
    let read = file.read(&mut buf).ok()?;
    digest.update(&buf[..read]);
    if size > 16384 {
        file.seek(SeekFrom::End(-8192)).ok()?;
        let read = file.read(&mut buf).ok()?;
        digest.update(&buf[..read]);
    }
    Some(to_hex(digest.finalize().as_bytes()))
}

fn full_hash(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut digest = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            break;
        }
        digest.update(&buf[..read]);
    }
    Some(to_hex(digest.finalize().as_bytes()))
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

// ---------- MFT 枚举 ----------

fn mft_exe() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("mftreader.exe"));
            candidates.push(dir.join("resources").join("mftreader.exe"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../mftreader.exe"));
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mftreader.exe"));
    candidates.into_iter().find(|path| path.exists())
}

fn cache_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(base).join("C盘清理工具").join("mftcache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn mft_files(root: &str, min_size: u64, skip_top: &[&str]) -> Vec<(String, u64)> {
    let Some(exe) = mft_exe() else {
        return Vec::new();
    };
    let output = Command::new(exe)
        .arg(root)
        .arg(min_size.to_string())
        .arg(cache_dir())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let root_len = root.trim_end_matches('\\').len() + 1;
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((size_text, path)) = line.split_once('\t') else {
            continue;
        };
        let Ok(size) = size_text.parse::<u64>() else {
            continue;
        };
        if size < min_size {
            continue;
        }
        let relative = if path.len() > root_len {
            &path[root_len..]
        } else {
            path
        };
        let first = relative.split('\\').next().unwrap_or("").to_lowercase();
        if skip_top.iter().any(|skip| *skip == first) {
            continue;
        }
        if path.split('\\').any(|part| part.starts_with('$')) {
            continue;
        }
        out.push((path.to_string(), size));
    }
    out
}

fn fixed_drives() -> Vec<String> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let mut drives = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + index as u8) as char);
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        if unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_FIXED {
            drives.push(root);
        }
    }
    if drives.is_empty() {
        drives.push("C:\\".to_string());
    }
    drives
}

fn file_mtime(path: &str) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------- 命令：删除 ----------

#[derive(Serialize)]
pub struct DeleteReport {
    pub freed: u64,
    pub deleted: u64,
    pub failed: Vec<String>,
    pub reasons: Vec<String>,
}

fn delete_report(
    title: &str,
    freed: u64,
    deleted: u64,
    failed: Vec<String>,
    reasons: Vec<String>,
) -> DeleteReport {
    let _ = logs::write_log(&[
        "开始清理".to_string(),
        format!("清理：{}", title),
        format!(
            "完成：删除 {} 个，释放 {} 字节，失败 {} 个",
            deleted,
            freed,
            failed.len()
        ),
    ]);
    DeleteReport {
        freed,
        deleted,
        failed,
        reasons,
    }
}

#[derive(Deserialize)]
struct VerifyItem {
    path: String,
    hash: String,
    size: u64,
    mtime: u64,
}

#[derive(Serialize)]
struct CleanReport {
    freed: u64,
    deleted: u64,
    skipped: u64,
    pending: u64,
    errors: Vec<String>,
    notes: Vec<String>,
}

#[tauri::command]
fn delete_recycle(paths: Vec<String>) -> DeleteReport {
    let title = "大文件";
    let mut eligible = Vec::new();
    let mut sizes: HashMap<String, u64> = HashMap::new();
    let mut failed = Vec::new();
    let mut reasons = Vec::new();
    for path in paths {
        if !Path::new(&path).exists() {
            failed.push(path.clone());
            reasons.push(format!("{}：文件已不存在", path));
            continue;
        }
        if safety::explicit_delete_forbidden(&path) {
            failed.push(path.clone());
            reasons.push(format!("{}：受保护路径或密码库，已跳过", path));
            continue;
        }
        let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        sizes.insert(path.clone(), size);
        eligible.push(path);
    }
    let _ = recycle::send_to_recycle(&eligible);
    let mut deleted = 0u64;
    let mut freed = 0u64;
    for path in &eligible {
        if Path::new(path).exists() {
            failed.push(path.clone());
            reasons.push(format!("{}：送回收站失败（可能被占用）", path));
        } else {
            deleted += 1;
            freed += sizes.get(path).copied().unwrap_or(0);
        }
    }
    invalidate_dup_cache();
    delete_report(title, freed, deleted, failed, reasons)
}

#[tauri::command]
fn delete_dups(items: Vec<VerifyItem>) -> DeleteReport {
    let title = "重复文件";
    let mut eligible = Vec::new();
    let mut sizes: HashMap<String, u64> = HashMap::new();
    let mut failed = Vec::new();
    let mut reasons = Vec::new();
    for item in items {
        let path = item.path;
        if !Path::new(&path).exists() {
            failed.push(path.clone());
            reasons.push(format!("{}：文件已不存在", path));
            continue;
        }
        if safety::explicit_delete_forbidden(&path) {
            failed.push(path.clone());
            reasons.push(format!("{}：受保护路径或密码库，已跳过", path));
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            failed.push(path.clone());
            reasons.push(format!("{}：读不到文件属性", path));
            continue;
        };
        if meta.len() != item.size {
            failed.push(path.clone());
            reasons.push(format!("{}：扫描后大小变了，已跳过", path));
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        if mtime != item.mtime {
            failed.push(path.clone());
            reasons.push(format!("{}：扫描后修改时间变了，已跳过", path));
            continue;
        }
        match full_hash(Path::new(&path)) {
            Some(hash) if hash == item.hash => {
                sizes.insert(path.clone(), item.size);
                eligible.push(path);
            }
            _ => {
                failed.push(path.clone());
                reasons.push(format!("{}：扫描后内容变了，已跳过", path));
            }
        }
    }
    let _ = recycle::send_to_recycle(&eligible);
    let mut deleted = 0u64;
    let mut freed = 0u64;
    for path in &eligible {
        if Path::new(path).exists() {
            failed.push(path.clone());
            reasons.push(format!("{}：送回收站失败（可能被占用）", path));
        } else {
            deleted += 1;
            freed += sizes.get(path).copied().unwrap_or(0);
        }
    }
    invalidate_dup_cache();
    delete_report(title, freed, deleted, failed, reasons)
}

#[tauri::command]
fn clean_categories(keys: Vec<String>) -> CleanReport {
    let mut report = CleanReport {
        freed: 0,
        deleted: 0,
        skipped: 0,
        pending: 0,
        errors: Vec::new(),
        notes: Vec::new(),
    };
    let rules = all_rules();
    let by_key: HashMap<&str, &Rule> = rules.iter().map(|rule| (rule.key.as_str(), rule)).collect();
    for key in keys {
        let Some(rule) = by_key.get(key.as_str()) else {
            report.errors.push(format!("找不到清理项：{}", key));
            continue;
        };
        let kind = if rule.kind.is_empty() {
            "files"
        } else {
            rule.kind.as_str()
        };
        if kind != "files" {
            report.notes.push(format!(
                "「{}」需要系统命令，Rust 版暂未迁移，已跳过",
                if rule.name.is_empty() {
                    key.clone()
                } else {
                    rule.name.clone()
                }
            ));
            continue;
        }
        let mut roots = expand_paths(&rule.paths);
        roots.extend(
            expand_paths(&rule.glob_dirs)
                .into_iter()
                .filter(|path| path.is_dir()),
        );
        roots.sort();
        roots.dedup();
        if roots.is_empty() {
            continue;
        }
        let root_strings: Vec<String> = roots
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        let include = build_matcher(&rule.patterns);
        let excludes: Vec<String> = rule
            .exclude
            .iter()
            .map(|item| normalize_exclude(item))
            .collect();
        let exclude = build_matcher(&excludes);
        let min_age = Duration::from_secs(rule.min_age_minutes.saturating_mul(60));

        for root in &root_strings {
            if safety::is_reparse_point(root) {
                report.skipped += 1;
                continue;
            }
            if Path::new(root).is_file() {
                clean_one(
                    root,
                    &root_strings,
                    &include,
                    &exclude,
                    min_age,
                    &mut report,
                );
                continue;
            }
            for entry in WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if !include_matches(&name, &include, &exclude) {
                    continue;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if !age_ok(&meta, min_age, SystemTime::now()) {
                    continue;
                }
                clean_one(
                    &entry.path().to_string_lossy(),
                    &root_strings,
                    &include,
                    &exclude,
                    min_age,
                    &mut report,
                );
            }
            if rule.prune_dirs && !safety::is_never_clean(root) {
                let removed = safety::remove_empty_dirs(root);
                if removed > 0 {
                    report.notes.push(format!("移除空目录 {} 个", removed));
                }
            }
        }
    }
    if report.errors.len() > 10 {
        report.errors.truncate(10);
    }
    let _ = logs::write_log(&[
        "开始清理".to_string(),
        "清理：C盘瘦身".to_string(),
        format!(
            "完成：删除 {} 个文件，释放 {} 字节，跳过 {} 个，待重启 {} 个",
            report.deleted, report.freed, report.skipped, report.pending
        ),
    ]);
    report
}

fn clean_one(
    path: &str,
    roots: &[String],
    include: &[glob::Pattern],
    exclude: &[glob::Pattern],
    min_age: Duration,
    report: &mut CleanReport,
) {
    let name = safety::basename(path).to_lowercase();
    if !include_matches(&name, include, exclude) {
        return;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        report.skipped += 1;
        return;
    };
    if !age_ok(&meta, min_age, SystemTime::now()) {
        return;
    }
    let size = meta.len();
    match safety::delete_file_checked(path, roots, true) {
        safety::DeleteOutcome::Deleted => {
            report.deleted += 1;
            report.freed += size;
        }
        safety::DeleteOutcome::Pending => {
            report.pending += 1;
            report.skipped += 1;
        }
        safety::DeleteOutcome::Skipped => report.skipped += 1,
        safety::DeleteOutcome::Failed(message) => {
            report.skipped += 1;
            if report.errors.len() < 20 {
                report.errors.push(message);
            }
        }
    }
}

// ---------- 命令：碎片清理 ----------

#[tauri::command]
fn list_drives() -> Vec<String> {
    fixed_drives()
}

#[tauri::command]
fn frag_analyze(drive: String) -> frag::FragReport {
    frag::analyze(&drive)
}

#[tauri::command]
fn frag_optimize(drive: String) -> Result<(), String> {
    frag::optimize(&drive)
}

// ---------- 命令：微信 / QQ ----------

#[tauri::command]
fn im_roots(kind: String) -> Vec<String> {
    im::roots(&kind)
}

#[tauri::command]
fn im_scan(kind: String) -> Vec<im::ImRecord> {
    im::list_media(&im::roots(&kind))
}

#[tauri::command]
fn im_clean(kind: String, kinds: Vec<String>, paths: Option<Vec<String>>) -> DeleteReport {
    im::clean(&kind, &kinds, paths)
}

// ---------- 命令：隐私清理 ----------

#[tauri::command]
fn privacy_scan() -> Vec<privacy::PrivacyItem> {
    privacy::scan()
}

#[tauri::command]
fn privacy_clean(ids: Vec<String>) -> privacy::PrivacyReport {
    privacy::clean(ids)
}

// ---------- 命令：粉碎 / 清理历史 / 规则更新 ----------

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[tauri::command]
fn shred(paths: Vec<String>, passes: Option<u32>) -> shred::ShredReport {
    let report = shred::shred_paths(paths, passes.unwrap_or(1));
    logs::append_line(
        "shred_history.log",
        &format!(
            "{}\tok\t成功 {} 个，失败 {} 个",
            timeutil::human_stamp(now_seconds()),
            report.files_ok,
            report.files_failed
        ),
    );
    report
}

#[tauri::command]
fn history_list() -> Vec<logs::LogEntry> {
    logs::list()
}

#[tauri::command]
fn history_read(name: String) -> Result<String, String> {
    logs::read(&name)
}

#[tauri::command]
fn rules_update() -> Result<String, String> {
    rules::update().map(|(added, total, note)| {
        format!(
            "新增 {} 条，共 {} 条。{}\n下次扫描生效。",
            added, total, note
        )
    })
}

#[tauri::command]
fn rules_rollback() -> Result<String, String> {
    rules::rollback()
}

// ---------- 用户目录 ----------

#[tauri::command]
fn user_profile() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".to_string())
}

// ---------- 启动 ----------

// 读取 $MFT 需要管理员权限，不是管理员时以 runas 重新启动自己。
// 直接声明 shell32 的导出，避免为两个函数引入整套 windows-sys feature。
#[link(name = "shell32")]
extern "system" {
    fn IsUserAnAdmin() -> i32;
    fn ShellExecuteW(
        hwnd: *mut core::ffi::c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> *mut core::ffi::c_void;
}

fn ensure_admin() {
    if unsafe { IsUserAnAdmin() } != 0 {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };
    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    if result as isize > 32 {
        std::process::exit(0);
    }
}

/// 命令行自检：不打开窗口，直接跑一遍后端扫描，方便在没有界面的环境里验证。
fn run_selftest() {
    let start = std::time::Instant::now();
    match disk_info() {
        Ok(info) => println!(
            "disk: total={} used={} free={}",
            info.total, info.used, info.free
        ),
        Err(error) => println!("disk: 失败 {}", error),
    }
    let started = std::time::Instant::now();
    let categories = scan_categories();
    let total: i64 = categories.iter().map(|cat| cat.size.max(0)).sum();
    println!(
        "categories: {} 条, 合计 {} 字节, 用时 {:.1}s",
        categories.len(),
        total,
        started.elapsed().as_secs_f64()
    );
    let started = std::time::Instant::now();
    let big = scan_big(None);
    println!(
        "big: {} 个, 用时 {:.1}s",
        big.len(),
        started.elapsed().as_secs_f64()
    );
    let started = std::time::Instant::now();
    let dups = scan_dups(Some("C:\\".to_string()), None);
    println!(
        "dups: {} 组, 用时 {:.1}s",
        dups.len(),
        started.elapsed().as_secs_f64()
    );
    let started = std::time::Instant::now();
    let report = frag::analyze("C:\\");
    println!(
        "frag: 碎片率={:?} 碎片数={:?} 碎片空间={:?}GB 可用={:?}GB 固态={:?}, 用时 {:.1}s",
        report.percent,
        report.count,
        report.space_gb,
        report.free_gb,
        report.ssd,
        started.elapsed().as_secs_f64()
    );
    let started = std::time::Instant::now();
    let wechat = im::list_media(&im::roots("wechat"));
    let qq = im::list_media(&im::roots("qq"));
    println!(
        "im: 微信 {} 个文件, QQ {} 个文件, 用时 {:.1}s",
        wechat.len(),
        qq.len(),
        started.elapsed().as_secs_f64()
    );
    let started = std::time::Instant::now();
    let privacy = privacy::scan();
    let with_data = privacy.iter().filter(|item| item.count > 0).count();
    println!(
        "privacy: {} 项, 有数据 {} 项, 用时 {:.1}s",
        privacy.len(),
        with_data,
        started.elapsed().as_secs_f64()
    );
    for item in privacy.iter().filter(|item| item.count > 0) {
        println!("  privacy: {} = {} 条", item.name, item.count);
    }
    println!("合计用时 {:.1}s", start.elapsed().as_secs_f64());
}

fn main() {
    // 磁盘扫描是 I/O 密集操作，线程开太多反而被磁盘和杀毒软件拖垮。
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build_global();
    if std::env::args().any(|arg| arg == "--selftest") {
        run_selftest();
        return;
    }
    ensure_admin();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            disk_info,
            scan_status,
            cancel_scan,
            scan_categories,
            scan_big,
            scan_dups,
            user_profile,
            delete_recycle,
            delete_dups,
            clean_categories,
            list_drives,
            frag_analyze,
            frag_optimize,
            im_roots,
            im_scan,
            im_clean,
            shred,
            history_list,
            history_read,
            rules_update,
            rules_rollback,
            privacy_scan,
            privacy_clean
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod delete_tests {
    use super::*;

    fn sandbox(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn recycle_moves_file_to_bin() {
        let dir = sandbox("cdc-recycle-test");
        let file = dir.join("big.bin");
        std::fs::write(&file, vec![0u8; 4096]).unwrap();
        let report = delete_recycle(vec![file.to_string_lossy().to_string()]);
        assert_eq!(report.deleted, 1);
        assert_eq!(report.freed, 4096);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_delete_rechecks_hash_and_keeps_changed_files() {
        let dir = sandbox("cdc-dup-test");
        let first = dir.join("a.bin");
        let second = dir.join("b.bin");
        std::fs::write(&first, b"same-content").unwrap();
        std::fs::write(&second, b"same-content").unwrap();
        let meta = std::fs::metadata(&first).unwrap();
        let size = meta.len();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let hash = full_hash(&first).unwrap();

        let good = VerifyItem {
            path: first.to_string_lossy().to_string(),
            hash: hash.clone(),
            size,
            mtime,
        };
        // 哈希对不上的条目必须保留。
        let bad = VerifyItem {
            path: second.to_string_lossy().to_string(),
            hash: "0000".to_string(),
            size,
            mtime,
        };
        let report = delete_dups(vec![good, bad]);
        assert_eq!(report.deleted, 1);
        assert!(!first.exists());
        assert!(second.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
