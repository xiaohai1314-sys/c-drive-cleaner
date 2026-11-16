#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

mod fastio;
mod fileindex;
mod frag;
mod hashcache;
mod mft;
mod im;
mod indextask;
mod logs;
mod privacy;
mod process;
mod recycle;
mod rules;
mod safety;
mod shred;
mod system;
mod timeutil;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;


const RULES_JSON: &str = include_str!("../../rules.json");
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_RANDOM_ACCESS: u32 = 0x1000_0000;
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;

#[derive(Serialize, Clone)]
struct ScanStatus {
    active: bool,
    stage: String,
    cancelled: bool,
    done: u64,
    total: u64,
    found: u64,
}

// 重复文件扫描的进度用原子计数，避免每个文件都去抢状态锁。
static DUP_TOTAL: AtomicU64 = AtomicU64::new(0);
static DUP_DONE: AtomicU64 = AtomicU64::new(0);
static DUP_FOUND: AtomicU64 = AtomicU64::new(0);
static DUP_ENUM_DONE: AtomicU64 = AtomicU64::new(0);
// 取消标志也用原子变量：哈希阶段每个文件都要查一次，不能走互斥锁。
static DUP_CANCEL: AtomicBool = AtomicBool::new(false);

fn dup_progress_reset(total: usize) {
    DUP_TOTAL.store(total as u64, Ordering::Relaxed);
    DUP_DONE.store(0, Ordering::Relaxed);
    DUP_FOUND.store(0, Ordering::Relaxed);
}

fn dup_progress_tick() {
    DUP_DONE.fetch_add(1, Ordering::Relaxed);
}

fn dup_progress_found() {
    DUP_FOUND.fetch_add(1, Ordering::Relaxed);
}

fn scan_stage(kind: &str, stage: &str) {
    if let Ok(mut map) = scan_status_cell().lock() {
        if let Some(status) = map.get_mut(kind) {
            status.stage = stage.to_string();
        }
    }
}

static SCAN_STATUS: OnceLock<Mutex<HashMap<String, ScanStatus>>> = OnceLock::new();

fn scan_status_cell() -> &'static Mutex<HashMap<String, ScanStatus>> {
    SCAN_STATUS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn scan_begin(kind: &str, stage: &str) {
    if kind == "dup" {
        DUP_CANCEL.store(false, Ordering::Relaxed);
        dup_progress_reset(0);
        DUP_ENUM_DONE.store(0, Ordering::Relaxed);
    }
    if let Ok(mut map) = scan_status_cell().lock() {
        map.insert(
            kind.to_string(),
            ScanStatus {
                active: true,
                stage: stage.to_string(),
                cancelled: false,
                done: 0,
                total: 0,
                found: 0,
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

fn dup_cancelled() -> bool {
    DUP_CANCEL.load(Ordering::Relaxed) || scan_was_cancelled("dup")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DiskKey {
    Ssd,
    HddPhysical(u32, u32),
    HddDrive(char),
}

/// 查询某个路径所在物理磁盘类型与标识（按盘符缓存）。
/// 固态盘合并走并行池；机械盘按物理盘号分组，同盘多分区共用同一串行队列，不同物理机械盘之间并行。
fn disk_key_for_path(path: &str, cache: &mut HashMap<char, DiskKey>) -> DiskKey {
    let letter = path.chars().next().unwrap_or('C').to_ascii_uppercase();
    *cache.entry(letter).or_insert_with(|| {
        let drive = format!("{}:\\", letter);
        if frag::is_ssd(&drive).unwrap_or(true) {
            DiskKey::Ssd
        } else if let Some((dt, dn)) = frag::physical_disk_id(&drive) {
            DiskKey::HddPhysical(dt, dn)
        } else {
            DiskKey::HddDrive(letter)
        }
    })
}

/// 按物理磁盘用独立 OS 线程并发枚举（不占用全局 Rayon 池），
/// 避免内部调用 `par_chunks_exact` 时与外层 Rayon 任务形成线程池饥饿死锁。
fn run_per_physical_disk<T, R, F>(
    items: Vec<T>,
    path_of: impl Fn(&T) -> &str,
    worker: F,
) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync + Send,
{
    if items.is_empty() {
        return Vec::new();
    }
    let mut disk_cache: HashMap<char, DiskKey> = HashMap::new();
    let mut ssd_items: Vec<T> = Vec::new();
    let mut hdd_groups: HashMap<DiskKey, Vec<T>> = HashMap::new();

    for item in items {
        match disk_key_for_path(path_of(&item), &mut disk_cache) {
            DiskKey::Ssd => ssd_items.push(item),
            hdd_key => hdd_groups.entry(hdd_key).or_default().push(item),
        }
    }

    let worker_ref = &worker;
    let mut results = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(ssd_items.len() + hdd_groups.len());
        for item in ssd_items {
            handles.push(scope.spawn(move || vec![worker_ref(item)]));
        }
        for (_, queue) in hdd_groups {
            handles.push(scope.spawn(move || {
                let mut out = Vec::with_capacity(queue.len());
                for item in queue {
                    out.push(worker_ref(item));
                }
                out
            }));
        }
        for h in handles {
            if let Ok(part) = h.join() {
                results.extend(part);
            }
        }
    });
    results
}

fn run_disk_aware<T, R, F>(
    items: Vec<T>,
    path_of: impl Fn(&T) -> &str,
    sort_key_of: impl Fn(&T) -> u64,
    worker: F,
) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync + Send,
{
    run_disk_aware_ncq(items, path_of, sort_key_of, 1, worker)
}

/// 按物理磁盘调度的任务执行器：
/// - SSD 任务直接走 Rayon 全核并行池；
/// - 每块物理机械盘各自独立运行（不同物理机械盘之间并行），盘内按 `sort_key`（LCN/FRN）升序排队；
/// - `hdd_workers` 控制每块物理机械盘的浅队列并发度（随机小块抽样用 2 利用 NCQ，大文件顺序读用 1）。
fn run_disk_aware_ncq<T, R, F>(
    items: Vec<T>,
    path_of: impl Fn(&T) -> &str,
    sort_key_of: impl Fn(&T) -> u64,
    hdd_workers: usize,
    worker: F,
) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync + Send,
{
    if items.is_empty() {
        return Vec::new();
    }
    let mut disk_cache: HashMap<char, DiskKey> = HashMap::new();
    let mut ssd_items: Vec<T> = Vec::new();
    let mut hdd_groups: HashMap<DiskKey, Vec<T>> = HashMap::new();

    for item in items {
        match disk_key_for_path(path_of(&item), &mut disk_cache) {
            DiskKey::Ssd => ssd_items.push(item),
            hdd_key => hdd_groups.entry(hdd_key).or_default().push(item),
        }
    }

    if hdd_groups.is_empty() {
        return ssd_items.into_par_iter().map(&worker).collect();
    }

    let mut hdd_queues: Vec<Vec<T>> = hdd_groups.into_values().collect();
    for queue in &mut hdd_queues {
        queue.sort_by(|a, b| {
            sort_key_of(a)
                .cmp(&sort_key_of(b))
                .then_with(|| path_of(a).cmp(path_of(b)))
        });
    }

    let workers_per_hdd = hdd_workers.clamp(1, 4);
    let mut results: Vec<R> = Vec::new();
    let worker_ref = &worker;
    std::thread::scope(|scope| {
        let mut hdd_handles = Vec::with_capacity(hdd_queues.len() * workers_per_hdd);
        for queue in hdd_queues {
            if workers_per_hdd == 1 || queue.len() <= 2 {
                hdd_handles.push(scope.spawn(move || {
                    let mut out = Vec::with_capacity(queue.len());
                    for item in queue {
                        out.push(worker_ref(item));
                    }
                    out
                }));
            } else {
                let shared = std::sync::Arc::new(Mutex::new(VecDeque::from(queue)));
                for _ in 0..workers_per_hdd {
                    let q = std::sync::Arc::clone(&shared);
                    hdd_handles.push(scope.spawn(move || {
                        let mut out = Vec::new();
                        loop {
                            let next = q.lock().ok().and_then(|mut guard| guard.pop_front());
                            let Some(item) = next else {
                                break;
                            };
                            out.push(worker_ref(item));
                        }
                        out
                    }));
                }
            }
        }
        if !ssd_items.is_empty() {
            results = ssd_items.into_par_iter().map(worker_ref).collect();
        }
        for handle in hdd_handles {
            if let Ok(part) = handle.join() {
                results.extend(part);
            }
        }
    });
    results
}

#[tauri::command]
fn scan_status() -> HashMap<String, ScanStatus> {
    let mut map = scan_status_cell()
        .lock()
        .map(|map| map.clone())
        .unwrap_or_default();
    if let Some(status) = map.get_mut("dup") {
        status.done = DUP_DONE.load(Ordering::Relaxed);
        status.total = DUP_TOTAL.load(Ordering::Relaxed);
        status.found = DUP_FOUND.load(Ordering::Relaxed);
    }
    map
}

/// 不传 kind 时取消所有正在跑的扫描。
#[tauri::command]
fn cancel_scan(kind: Option<String>) {
    if kind.as_deref().map(|kind| kind == "dup").unwrap_or(true) {
        DUP_CANCEL.store(true, Ordering::Relaxed);
    }
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

#[tauri::command(async)]
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

#[tauri::command(async)]
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

/// 大文件按类型分档阈值（字节），取自毒霸 kbigfile.dat（v2024.07.12.1，12 条 suffix 规则）。
/// 分组与前端 kindOf 的页签对齐；未列出的扩展名走兜底 50MB（毒霸 id 120 的 `*` 规则）。
/// 毒霸的 gif 同时出现在视频（20MB）和图片（2MB）两条规则里，这里按前端的归类算图片 2MB。
fn big_file_threshold_bytes(path: &str) -> u64 {
    const MB: u64 = 1024 * 1024;
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        // 安装包 100MB（毒霸 id 1：exe|msi）
        "exe" | "msi" => 100 * MB,
        // 视频 20MB（毒霸 id 4）
        "mp4" | "avi" | "mov" | "mkv" | "wmv" | "flv" | "m4v" | "3gp" | "mpeg" | "mpg" | "ts"
        | "webm" | "rmvb" | "vob" | "dv" | "swf" => 20 * MB,
        // 临时文件 20MB（毒霸 id 9：tmp）
        "tmp" => 20 * MB,
        // 音频 5MB（毒霸 id 5）
        "mp3" | "ape" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "ac3" | "dts" | "mka" | "opus"
        | "wma" | "mid" => 5 * MB,
        // 压缩包 5MB（毒霸 id 2）
        "zip" | "rar" | "7z" | "gz" | "iso" | "img" | "tar" | "bz2" | "xz" | "cab" | "fzip"
        | "rar4" => 5 * MB,
        // 手机安装包 5MB（毒霸 id 3：apk|ipa|ipsw）
        "apk" | "ipa" | "ipsw" => 5 * MB,
        // 其他打包/调试文件 5MB（毒霸 id 119：pack|wim|pima|lib|pdb|rb|torrent）
        "pack" | "wim" | "pima" | "lib" | "pdb" | "rb" | "torrent" => 5 * MB,
        // 图片 2MB（毒霸 id 118）
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tiff" | "tif" | "webp" | "heic" | "ico"
        | "raw" | "psd" | "svg" => 2 * MB,
        // 文档 2MB（毒霸 id 8）
        "txt" | "csv" | "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odf"
        | "wps" | "wpt" | "dot" | "dotx" | "docm" | "xlsm" | "rtf" | "odt" | "dwg" | "dxf"
        | "md" | "et" | "dps" | "ofd" | "xmind" | "vsdx" => 2 * MB,
        // 日志 2MB（毒霸 id 6）
        "log" | "xlog" | "dump" | "dmp" | "lg" | "tlg" | "ttd" | "xltd" => 2 * MB,
        // 兜底 50MB（毒霸 id 120 的 `*` 规则）
        _ => 50 * MB,
    }
}

#[tauri::command(async)]
fn scan_big(drives: Option<Vec<String>>) -> Vec<BigFile> {
    scan_begin("big", "大文件扫描");
    let list = drives.unwrap_or_else(fixed_drives);
    // 索引优先：新鲜（必要时先做纯元数据增量）时直接查库，不枚举。
    // 索引缺失/断档时回落到下面的实时扫描，不触发回填。
    scan_stage("big", "索引同步");
    if let Some(index) = fileindex::FileIndex::open_default() {
        let mut index = index;
        if let Some(rows) = query_big_from_index(&mut index, &list) {
            scan_end("big");
            return rows;
        }
    }
    let per_drive = run_per_physical_disk(
        list,
        |d| d.as_str(),
        |drive| {
            if scan_was_cancelled("big") {
                return Vec::new();
            }
            // 按最低档 2MB（文档/图片/日志）枚举；分档阈值在 MFT 层按文件名先过滤，
            // 不达标的文件不再解析父目录、拼接完整路径。
            mft::scan_files_where(&drive, 2 * 1024 * 1024, &TOP_SKIP, &|name, size| {
                size >= big_file_threshold_bytes(name)
            })
        },
    );
    let mut rows: Vec<BigFile> = per_drive
        .into_iter()
        .flatten()
        .filter(|entry| !is_system_file_name(&entry.path))
        .filter(|entry| entry.size >= big_file_threshold_bytes(&entry.path))
        .map(|entry| BigFile {
            mtime: if entry.mtime != 0 {
                entry.mtime
            } else {
                file_mtime(&entry.path)
            },
            path: entry.path,
            size: entry.size,
        })
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

struct DupCandidate {
    path: String,
    size: u64,
    mtime: u64,
    sort_key: u64,
    frn: u64,
    first_lcn: u64,
    bytes_per_cluster: u32,
    can_raw_read: bool,
    is_ssd: bool,
    file_id: Option<(u32, u64)>,
    head: Option<hashcache::Digest>,
    sample: Option<hashcache::Digest>,
    strong: Option<hashcache::Digest>,
    full: Option<hashcache::Digest>,
    probe: Option<hashcache::Digest>,
    dirty_head: bool,
    dirty_sample: bool,
    dirty_strong: bool,
    dirty_full: bool,
}

struct PipelineOutput {
    final_items: Vec<DupCandidate>,
    eliminated: Vec<DupCandidate>,
}

fn build_dup_groups(
    by_hash: HashMap<(u64, hashcache::Digest), Vec<DupCandidate>>,
) -> Vec<DupGroup> {
    let mut groups: Vec<DupGroup> = Vec::new();
    // 只有候选组（≥2 个文件）才需要硬链接去重；缺 file_id 的条目并行补查，
    // 避免对缓存命中的大批文件逐个串行打开句柄。
    let mut by_hash: Vec<((u64, hashcache::Digest), Vec<DupCandidate>)> = by_hash
        .into_iter()
        .filter(|(_, items)| items.len() >= 2)
        .collect();
    by_hash.par_iter_mut().for_each(|(_, items)| {
        items.par_iter_mut().for_each(|item| {
            if item.file_id.is_none() {
                item.file_id = fastio::query_file_identity(Path::new(&item.path));
            }
        });
    });
    for ((size, digest), mut items) in by_hash {
        let mut seen_inodes: HashSet<(u32, u64)> = HashSet::new();
        items.retain(|item| {
            let fid = item.file_id;
            match fid {
                Some(id) => seen_inodes.insert(id),
                None => true,
            }
        });
        if items.len() < 2 {
            continue;
        }
        items.sort_by(|a, b| a.path.cmp(&b.path));
        let files: Vec<String> = items.iter().map(|item| item.path.clone()).collect();
        let mtimes: Vec<u64> = items.iter().map(|item| item.mtime).collect();
        dup_progress_found();
        groups.push(DupGroup {
            hash: hashcache::digest_to_hex(&digest),
            size,
            files,
            mtimes,
        });
    }
    groups
}

fn sort_dup_groups(groups: &mut [DupGroup]) {
    groups.sort_by(|a, b| {
        let left = a.size * a.files.len() as u64;
        let right = b.size * b.files.len() as u64;
        right.cmp(&left)
    });
}

/// FileEntry → DupCandidate（scan_dups 与回填共用）：摘要取自 file_hashes 缓存，
/// `mtime` 由调用方解析好（MFT 里没有时读文件属性）。
/// 索引行没有 MFT 布局信息，增量补算不走这条路（见 `hash_index_groups`）。
fn dup_candidate_from_file_entry(
    entry: mft::FileEntry,
    mtime: u64,
    cache: &hashcache::HashCache,
    disk_cache: &mut HashMap<char, DiskKey>,
) -> DupCandidate {
    let is_ssd = disk_key_for_path(&entry.path, disk_cache) == DiskKey::Ssd;
    let cached = cache.entry_for(&entry.path, entry.size, mtime);
    DupCandidate {
        path: entry.path,
        size: entry.size,
        mtime,
        sort_key: entry.sort_key,
        frn: entry.frn,
        first_lcn: entry.first_lcn,
        bytes_per_cluster: entry.bytes_per_cluster,
        can_raw_read: entry.can_raw_read,
        is_ssd,
        file_id: None,
        head: cached.and_then(|e| e.head),
        sample: cached.and_then(|e| e.sample),
        strong: cached.and_then(|e| e.strong),
        full: cached.and_then(|e| e.full),
        probe: None,
        dirty_head: false,
        dirty_sample: false,
        dirty_strong: false,
        dirty_full: false,
    }
}

/// 把按 size 分好组的候选按"是否已有最终摘要 / 是否全在 SSD 上"分流。
/// 返回（已就绪的最终组，纯 SSD 待跑管线，混合待跑管线）。
/// `fast` 为真时最终摘要是 strong（快速模式），否则是 full。
/// scan_dups、回填、增量补算三处共用（Phase 1 的重复逻辑，Phase 3 抽出）。
fn split_dup_candidates(
    by_size: HashMap<u64, Vec<DupCandidate>>,
    fast: bool,
) -> (
    HashMap<(u64, hashcache::Digest), Vec<DupCandidate>>,
    Vec<DupCandidate>,
    Vec<DupCandidate>,
) {
    let mut ready_final: HashMap<(u64, hashcache::Digest), Vec<DupCandidate>> = HashMap::new();
    let mut ssd_only_stage1: Vec<DupCandidate> = Vec::new();
    let mut mixed_stage1: Vec<DupCandidate> = Vec::new();
    for bucket in by_size.into_values() {
        if bucket.len() < 2 {
            continue;
        }
        let mut all_ready = true;
        let mut all_ssd = true;
        for item in &bucket {
            if !item.is_ssd {
                all_ssd = false;
            }
            let has_final = if fast {
                item.strong.is_some()
            } else {
                item.full.is_some()
            };
            if !has_final {
                all_ready = false;
            }
        }
        if all_ready {
            for item in bucket {
                let key_digest = if fast {
                    item.strong.unwrap()
                } else {
                    item.full.unwrap()
                };
                ready_final
                    .entry((item.size, key_digest))
                    .or_default()
                    .push(item);
            }
        } else if all_ssd {
            ssd_only_stage1.extend(bucket);
        } else {
            mixed_stage1.extend(bucket);
        }
    }
    (ready_final, ssd_only_stage1, mixed_stage1)
}

/// SSD/混合两路管线并行跑完，摘要回写缓存，返回（最终组候选，淘汰候选）。
/// `update_stage` 为真时更新 UI 阶段（实时扫描用；后台索引用 false）。
/// `fast`/`trace`/标签透传给 `run_dup_pipeline`。三处共用。
#[allow(clippy::too_many_arguments)]
fn run_dup_pipelines(
    ssd_stage1: Vec<DupCandidate>,
    mixed_stage1: Vec<DupCandidate>,
    cache: &mut hashcache::HashCache,
    stamp_now: u64,
    fast: bool,
    trace: bool,
    ssd_label: &str,
    mixed_label: &str,
    update_stage: bool,
) -> (Vec<DupCandidate>, Vec<DupCandidate>) {
    if ssd_stage1.is_empty() && mixed_stage1.is_empty() {
        return (Vec::new(), Vec::new());
    }
    dup_progress_reset(ssd_stage1.len() + mixed_stage1.len());
    // 消除栅栏：纯固态盘的候选桶独立完整跑完 1→2→3 阶段，无需等待慢速机械盘每一阶段的同步点。
    let has_mixed = !mixed_stage1.is_empty();
    let (ssd_out, mixed_out) = std::thread::scope(|scope| {
        let ssd_handle = if !ssd_stage1.is_empty() {
            Some(scope.spawn(move || {
                run_dup_pipeline(
                    ssd_stage1,
                    fast,
                    1,
                    update_stage && !has_mixed,
                    trace,
                    ssd_label,
                )
            }))
        } else {
            None
        };
        let mixed_out = if has_mixed {
            run_dup_pipeline(mixed_stage1, fast, 2, update_stage, trace, mixed_label)
        } else {
            PipelineOutput {
                final_items: Vec::new(),
                eliminated: Vec::new(),
            }
        };
        let ssd_out = ssd_handle
            .and_then(|h| h.join().ok())
            .unwrap_or(PipelineOutput {
                final_items: Vec::new(),
                eliminated: Vec::new(),
            });
        (ssd_out, mixed_out)
    });
    flush_candidate_hashes(cache, &ssd_out.eliminated, stamp_now);
    flush_candidate_hashes(cache, &ssd_out.final_items, stamp_now);
    flush_candidate_hashes(cache, &mixed_out.eliminated, stamp_now);
    flush_candidate_hashes(cache, &mixed_out.final_items, stamp_now);
    cache.save_dirty();
    let mut final_items = ssd_out.final_items;
    final_items.extend(mixed_out.final_items);
    let mut eliminated = ssd_out.eliminated;
    eliminated.extend(mixed_out.eliminated);
    (final_items, eliminated)
}

fn flush_candidate_hashes(
    cache: &mut hashcache::HashCache,
    items: &[DupCandidate],
    stamp_now: u64,
) {
    for c in items {
        if !(c.dirty_head || c.dirty_sample || c.dirty_strong || c.dirty_full) {
            continue;
        }
        cache.remember_digests(
            c.path.clone(),
            c.size,
            c.mtime,
            if c.dirty_head { c.head } else { None },
            if c.dirty_sample { c.sample } else { None },
            if c.dirty_strong { c.strong } else { None },
            if c.dirty_full { c.full } else { None },
            stamp_now,
        );
    }
}

fn run_dup_pipeline(
    stage1_items: Vec<DupCandidate>,
    fast: bool,
    hdd_ncq_workers: usize,
    update_stage: bool,
    trace: bool,
    label: &str,
) -> PipelineOutput {
    let mut eliminated: Vec<DupCandidate> = Vec::new();
    if stage1_items.is_empty() {
        return PipelineOutput {
            final_items: Vec::new(),
            eliminated,
        };
    }

    // 阶段 1：先尝试卷句柄按物理 LCN 合并直读（绕过路径解析与逐文件 Hook），
    // 剩余未覆盖的项再走 OpenFileById + Windows IOCP 异步重叠读取。
    if update_stage {
        scan_stage("dup", "初步筛选");
    }
    let mut stage1_done = stage1_items;
    let mut raw_by_drive: HashMap<char, (bool, Vec<fastio::RawHeadTarget>)> = HashMap::new();
    for (idx, c) in stage1_done.iter().enumerate() {
        if c.head.is_some() {
            dup_progress_tick();
            continue;
        }
        if c.can_raw_read && c.bytes_per_cluster > 0 && c.first_lcn > 0 {
            let letter = c.path.chars().next().unwrap_or('C').to_ascii_uppercase();
            let offset = c.first_lcn.saturating_mul(c.bytes_per_cluster as u64);
            let want = HEAD_CHUNK.min(c.size) as usize;
            let entry = raw_by_drive
                .entry(letter)
                .or_insert_with(|| (c.is_ssd, Vec::new()));
            entry.1.push(fastio::RawHeadTarget { idx, offset, want });
        }
    }

    if !raw_by_drive.is_empty() && !DUP_CANCEL.load(Ordering::Relaxed) {
        let drive_jobs: Vec<(char, String, bool, Vec<fastio::RawHeadTarget>)> = raw_by_drive
            .into_iter()
            .map(|(ch, (is_ssd, targets))| (ch, format!("{}:\\", ch), is_ssd, targets))
            .collect();
        let raw_batches = run_disk_aware(
            drive_jobs,
            |(_, root, _, _)| root.as_str(),
            |_| 0,
            |(ch, _, is_ssd, targets)| {
                fastio::coalesced_raw_volume_heads(ch, targets, is_ssd, &DUP_CANCEL, &dup_progress_tick)
            },
        );
        for batch in raw_batches {
            for (idx, digest) in batch {
                let c = &mut stage1_done[idx];
                c.head = Some(digest);
                c.dirty_head = true;
                if c.size <= HEAD_CHUNK {
                    c.sample = Some(digest);
                    c.strong = Some(digest);
                    c.full = Some(digest);
                    c.dirty_sample = true;
                    c.dirty_strong = true;
                    c.dirty_full = true;
                }
            }
        }
    }

    // 未命中卷句柄直读的项（如近期修改/加密/压缩/非 NTFS 或降级遍历），走 OpenFileById + IOCP 异步批读。
    let mut disk_cache: HashMap<char, DiskKey> = HashMap::new();
    let mut ssd_iocp: Vec<fastio::IocpHeadRequest> = Vec::new();
    let mut hdd_iocp: HashMap<DiskKey, Vec<(u64, fastio::IocpHeadRequest)>> = HashMap::new();
    for (idx, c) in stage1_done.iter().enumerate() {
        if c.head.is_some() {
            continue;
        }
        let req = fastio::IocpHeadRequest {
            idx,
            path: c.path.clone(),
            frn: c.frn,
            want: HEAD_CHUNK.min(c.size) as usize,
        };
        match disk_key_for_path(&c.path, &mut disk_cache) {
            DiskKey::Ssd => ssd_iocp.push(req),
            hdd_key => hdd_iocp.entry(hdd_key).or_default().push((c.sort_key, req)),
        }
    }

    if !ssd_iocp.is_empty() || !hdd_iocp.is_empty() {
        let mut hdd_lists: Vec<Vec<fastio::IocpHeadRequest>> = hdd_iocp
            .into_values()
            .map(|mut list| {
                list.sort_by_key(|(sk, _)| *sk);
                list.into_iter().map(|(_, req)| req).collect()
            })
            .collect();
        let iocp_batches = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(hdd_lists.len());
            for list in hdd_lists.drain(..) {
                handles.push(scope.spawn(move || {
                    fastio::iocp_read_heads(list, 8, &DUP_CANCEL, &dup_progress_tick)
                }));
            }
            let ssd_res = if !ssd_iocp.is_empty() {
                fastio::iocp_read_heads(ssd_iocp, 32, &DUP_CANCEL, &dup_progress_tick)
            } else {
                Vec::new()
            };
            let mut all = ssd_res;
            for h in handles {
                if let Ok(part) = h.join() {
                    all.extend(part);
                }
            }
            all
        });
        for (idx, digest, file_id) in iocp_batches {
            let c = &mut stage1_done[idx];
            if c.file_id.is_none() {
                c.file_id = file_id;
            }
            c.head = Some(digest);
            c.dirty_head = true;
            if c.size <= HEAD_CHUNK {
                c.sample = Some(digest);
                c.strong = Some(digest);
                c.full = Some(digest);
                c.dirty_sample = true;
                c.dirty_strong = true;
                c.dirty_full = true;
            }
        }
    }

    if dup_cancelled() {
        return PipelineOutput {
            final_items: Vec::new(),
            eliminated: stage1_done,
        };
    }

    let mut by_head: HashMap<(u64, hashcache::Digest), Vec<DupCandidate>> = HashMap::new();
    for item in stage1_done {
        if let Some(h) = item.head {
            by_head.entry((item.size, h)).or_default().push(item);
        }
    }
    let mut stage2_items: Vec<DupCandidate> = Vec::new();
    for (_, mut group) in by_head {
        let mut seen_inodes: HashSet<(u32, u64)> = HashSet::new();
        group.retain(|item| match item.file_id {
            Some(fid) => seen_inodes.insert(fid),
            None => true,
        });
        if group.len() >= 2 {
            stage2_items.extend(group);
        } else {
            eliminated.extend(group);
        }
    }

    if trace {
        eprintln!("dup[{}]: 4KB 初筛后剩余 {} 个文件", label, stage2_items.len());
    }
    if stage2_items.is_empty() {
        return PipelineOutput {
            final_items: Vec::new(),
            eliminated,
        };
    }

    if fast {
        let uncached_strong = stage2_items.iter().filter(|c| c.strong.is_none()).count();
        DUP_TOTAL.fetch_add(uncached_strong as u64, Ordering::Relaxed);
        if update_stage {
            scan_stage("dup", "快速比对");
        }
        let stage2_done = run_disk_aware_ncq(
            stage2_items,
            |c| c.path.as_str(),
            |c| c.sort_key,
            hdd_ncq_workers,
            |mut c| {
                if DUP_CANCEL.load(Ordering::Relaxed) {
                    return c;
                }
                if c.strong.is_none() {
                    if !c.is_ssd && c.size <= HDD_SEQ_THRESHOLD {
                        // 机械盘 <= 2MB：一次顺序读出全文同时算出 strong 与 full，免去 3 次寻道。
                        if let Some((strong_d, full_d, file_id)) =
                            read_hdd_seq_sample_and_full(Path::new(&c.path), c.frn, c.size, STRONG_CHUNK)
                        {
                            if c.file_id.is_none() {
                                c.file_id = file_id;
                            }
                            c.strong = Some(strong_d);
                            c.full = Some(full_d);
                            c.dirty_strong = true;
                            c.dirty_full = true;
                        }
                    } else if let Some((digest, file_id)) = strong_hash_by_frn(Path::new(&c.path), c.frn, c.size)
                    {
                        if c.file_id.is_none() {
                            c.file_id = file_id;
                        }
                        c.strong = Some(digest);
                        c.dirty_strong = true;
                        if c.size <= STRONG_CHUNK * 3 {
                            c.full = Some(digest);
                            c.dirty_full = true;
                        }
                    }
                    dup_progress_tick();
                }
                c
            },
        );

        if dup_cancelled() {
            eliminated.extend(stage2_done);
            return PipelineOutput {
                final_items: Vec::new(),
                eliminated,
            };
        }
        return PipelineOutput {
            final_items: stage2_done,
            eliminated,
        };
    }

    // 阶段 2（常规模式）：4KB 碰撞候选做采样比对（机械盘 <= 2MB 直接顺序一次读同时算出 sample 与 full）。
    let need_sample = stage2_items
        .iter()
        .filter(|c| c.sample.is_none() && c.full.is_none())
        .count();
    DUP_TOTAL.fetch_add(need_sample as u64, Ordering::Relaxed);
    if update_stage {
        scan_stage("dup", "采样比对");
    }
    let stage2_done = run_disk_aware_ncq(
        stage2_items,
        |c| c.path.as_str(),
        |c| c.sort_key,
        hdd_ncq_workers,
        |mut c| {
            if DUP_CANCEL.load(Ordering::Relaxed) {
                return c;
            }
            if c.sample.is_none() && c.full.is_none() {
                if !c.is_ssd && c.size <= HDD_SEQ_THRESHOLD {
                    if let Some((sample_d, full_d, file_id)) =
                        read_hdd_seq_sample_and_full(Path::new(&c.path), c.frn, c.size, SAMPLE_CHUNK)
                    {
                        if c.file_id.is_none() {
                            c.file_id = file_id;
                        }
                        c.sample = Some(sample_d);
                        c.full = Some(full_d);
                        c.dirty_sample = true;
                        c.dirty_full = true;
                    }
                } else if let Some((digest, file_id)) = sample_hash_by_frn(Path::new(&c.path), c.frn, c.size) {
                    if c.file_id.is_none() {
                        c.file_id = file_id;
                    }
                    c.sample = Some(digest);
                    c.dirty_sample = true;
                    if c.size <= SAMPLE_CHUNK * 3 {
                        c.full = Some(digest);
                        c.dirty_full = true;
                    }
                }
                dup_progress_tick();
            }
            c
        },
    );

    if dup_cancelled() {
        eliminated.extend(stage2_done);
        return PipelineOutput {
            final_items: Vec::new(),
            eliminated,
        };
    }

    let mut by_sample: HashMap<(u64, hashcache::Digest), Vec<DupCandidate>> = HashMap::new();
    let mut stage3_items: Vec<DupCandidate> = Vec::new();
    for item in stage2_done {
        if let Some(s) = item.sample {
            by_sample.entry((item.size, s)).or_default().push(item);
        } else if item.full.is_some() {
            stage3_items.push(item);
        }
    }
    for (_, group) in by_sample {
        if group.len() >= 2 {
            stage3_items.extend(group);
        } else {
            eliminated.extend(group);
        }
    }

    if trace {
        eprintln!("dup[{}]: 采样后仍需全校验 {} 个文件", label, stage3_items.len());
    }
    if stage3_items.is_empty() || std::env::var("CDC_DUP_SAMPLE_ONLY").is_ok() {
        eliminated.extend(stage3_items);
        return PipelineOutput {
            final_items: Vec::new(),
            eliminated,
        };
    }

    // 阶段 3 前置：对 >16MB 且全员尚未算过全哈希的碰撞组，先读 25% 与 75% 各 64KB 渐进探针，
    // 把头/中/尾碰巧相同但中间不同的大文件提前踢出，避免白白全盘读几个 GB。
    let mut probe_groups: HashMap<(u64, hashcache::Digest), Vec<DupCandidate>> = HashMap::new();
    let mut direct_stage3: Vec<DupCandidate> = Vec::new();
    for item in stage3_items {
        if item.size > PROBE_THRESHOLD {
            if let Some(s) = item.sample {
                probe_groups.entry((item.size, s)).or_default().push(item);
                continue;
            }
        }
        direct_stage3.push(item);
    }

    let mut need_probe: Vec<DupCandidate> = Vec::new();
    for (_, grp) in probe_groups {
        if grp.len() >= 2 && grp.iter().all(|c| c.full.is_none()) {
            need_probe.extend(grp);
        } else {
            direct_stage3.extend(grp);
        }
    }

    if !need_probe.is_empty() && !DUP_CANCEL.load(Ordering::Relaxed) {
        DUP_TOTAL.fetch_add(need_probe.len() as u64, Ordering::Relaxed);
        let probed = run_disk_aware_ncq(
            need_probe,
            |c| c.path.as_str(),
            |c| c.sort_key,
            hdd_ncq_workers,
            |mut c| {
                if DUP_CANCEL.load(Ordering::Relaxed) {
                    return c;
                }
                if let Some((digest, file_id)) = probe_hash(Path::new(&c.path), c.frn, c.size) {
                    if c.file_id.is_none() {
                        c.file_id = file_id;
                    }
                    c.probe = Some(digest);
                }
                dup_progress_tick();
                c
            },
        );
        if dup_cancelled() {
            eliminated.extend(direct_stage3);
            eliminated.extend(probed);
            return PipelineOutput {
                final_items: Vec::new(),
                eliminated,
            };
        }
        let mut by_probe: HashMap<(u64, hashcache::Digest), Vec<DupCandidate>> = HashMap::new();
        for item in probed {
            if let Some(p) = item.probe {
                by_probe.entry((item.size, p)).or_default().push(item);
            } else {
                direct_stage3.push(item);
            }
        }
        for (_, grp) in by_probe {
            if grp.len() >= 2 {
                direct_stage3.extend(grp);
            } else {
                eliminated.extend(grp);
            }
        }
    }

    if direct_stage3.is_empty() {
        return PipelineOutput {
            final_items: Vec::new(),
            eliminated,
        };
    }

    // 阶段 3：全文件顺序双缓冲校验（机械盘保持单路顺序流式读，避免多流争抢磁头）。
    let need_full = direct_stage3.iter().filter(|c| c.full.is_none()).count();
    DUP_TOTAL.fetch_add(need_full as u64, Ordering::Relaxed);
    if update_stage {
        scan_stage("dup", "校验内容");
    }

    let stage3_done = run_disk_aware_ncq(
        direct_stage3,
        |c| c.path.as_str(),
        |c| c.sort_key,
        1,
        |mut c| {
            if DUP_CANCEL.load(Ordering::Relaxed) {
                return c;
            }
            if c.full.is_none() {
                if let Some((digest, file_id)) = full_hash_by_frn(Path::new(&c.path), c.frn) {
                    if c.file_id.is_none() {
                        c.file_id = file_id;
                    }
                    c.full = Some(digest);
                    c.dirty_full = true;
                }
                dup_progress_tick();
            }
            c
        },
    );

    if dup_cancelled() {
        eliminated.extend(stage3_done);
        return PipelineOutput {
            final_items: Vec::new(),
            eliminated,
        };
    }

    PipelineOutput {
        final_items: stage3_done,
        eliminated,
    }
}

#[tauri::command(async)]
fn scan_dups(
    root: Option<String>,
    min_size: Option<u64>,
    drives: Option<Vec<String>>,
    fast: Option<bool>,
) -> Vec<DupGroup> {
    scan_begin("dup", "枚举文件");
    let fast = fast.unwrap_or(false);
    let trace = std::env::var("CDC_TRACE").is_ok();
    let fixed = fixed_drives();
    let roots: Vec<String> = match drives {
        Some(list) if !list.is_empty() => list
            .into_iter()
            .filter(|drive| fixed.iter().any(|item| item.eq_ignore_ascii_case(drive)))
            .collect(),
        _ => root.map(|value| vec![value]).unwrap_or(fixed),
    };
    if roots.is_empty() {
        scan_end("dup");
        return Vec::new();
    }
    let min = min_size.unwrap_or(1 << 20);
    // 索引优先：新鲜（必要时先增量追赶）且完整时直接查库，不枚举、不哈希。
    // 索引缺失/断档/不完整时回落到下面的 JSON 缓存与实时扫描。
    scan_stage("dup", "索引同步");
    if let Some(index) = fileindex::FileIndex::open_default() {
        let mut index = index;
        if let Some(groups) = query_dups_from_index(&mut index, &roots, min, fast) {
            scan_end("dup");
            return groups;
        }
    }
    let cache_key = blake3::hash(format!("{:?}:{}:{}", roots, min, fast).as_bytes())
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
    scan_stage("dup", "枚举文件");
    let drive_total = roots.len() as u64;
    let per_drive: Vec<Vec<mft::FileEntry>> = run_per_physical_disk(
        roots,
        |d| d.as_str(),
        |drive| {
            if DUP_CANCEL.load(Ordering::Relaxed) || scan_was_cancelled("dup") {
                return Vec::new();
            }
            let files = mft::scan_files(&drive, min, &TOP_SKIP);
            let done = DUP_ENUM_DONE.fetch_add(1, Ordering::Relaxed) + 1;
            scan_stage(
                "dup",
                &format!(
                    "枚举文件 {}/{}（{}）",
                    done,
                    drive_total,
                    drive.trim_end_matches('\\')
                ),
            );
            files
        },
    );

    if DUP_CANCEL.load(Ordering::Relaxed) || scan_was_cancelled("dup") {
        scan_end("dup");
        return Vec::new();
    }

    // 第一层快速过滤：大小唯一的文件直接剔除。
    let mut by_size: HashMap<u64, Vec<mft::FileEntry>> = HashMap::new();
    for files in per_drive {
        for entry in files {
            if is_system_file_name(&entry.path) {
                continue;
            }
            by_size.entry(entry.size).or_default().push(entry);
        }
    }

    let mut size_buckets: Vec<Vec<mft::FileEntry>> = by_size
        .into_values()
        .filter(|bucket| bucket.len() >= 2)
        .collect();
    let candidates_count: usize = size_buckets.iter().map(|b| b.len()).sum();

    if trace {
        eprintln!(
            "dup: 参与比对 {} 个文件, 枚举用时 {:.1}s",
            candidates_count,
            started.elapsed().as_secs_f64()
        );
    }
    if candidates_count == 0 {
        scan_end("dup");
        return Vec::new();
    }

    let mut path_list: Vec<String> = Vec::with_capacity(candidates_count);
    for bucket in &size_buckets {
        for entry in bucket {
            path_list.push(entry.path.clone());
        }
    }
    let mut cache = hashcache::HashCache::load_for_paths(path_list.iter().map(|s| s.as_str()));
    drop(path_list);
    let stamp_now = now_seconds();

    let mut disk_cache: HashMap<char, DiskKey> = HashMap::new();
    let mut by_size: HashMap<u64, Vec<DupCandidate>> = HashMap::new();
    for bucket in size_buckets.drain(..) {
        for entry in bucket {
            let mtime = if entry.mtime != 0 {
                entry.mtime
            } else {
                file_mtime(&entry.path)
            };
            let size = entry.size;
            let candidate = dup_candidate_from_file_entry(entry, mtime, &cache, &mut disk_cache);
            by_size.entry(size).or_default().push(candidate);
        }
    }
    let (mut ready_final, ssd_only_stage1, mixed_stage1) = split_dup_candidates(by_size, fast);

    let pending_total = ssd_only_stage1.len() + mixed_stage1.len();
    if pending_total == 0 {
        let mut groups = build_dup_groups(ready_final);
        sort_dup_groups(&mut groups);
        if !groups.is_empty() {
            if let Ok(text) = serde_json::to_string(&groups) {
                let _ = std::fs::write(&cache_path, text);
            }
        }
        scan_end("dup");
        return groups;
    }

    let (final_items, _eliminated) = run_dup_pipelines(
        ssd_only_stage1,
        mixed_stage1,
        &mut cache,
        stamp_now,
        fast,
        trace,
        "ssd",
        "hdd+mixed",
        true,
    );

    if dup_cancelled() {
        scan_end("dup");
        return Vec::new();
    }

    for item in final_items {
        let digest_opt = if fast { item.strong } else { item.full };
        if let Some(digest) = digest_opt {
            ready_final
                .entry((item.size, digest))
                .or_default()
                .push(item);
        }
    }

    let mut groups = build_dup_groups(ready_final);
    sort_dup_groups(&mut groups);

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

const HEAD_CHUNK: u64 = 4096;
const SAMPLE_CHUNK: u64 = 16 * 1024;
const STRONG_CHUNK: u64 = 256 * 1024;
const PROBE_CHUNK: u64 = 64 * 1024;
const PROBE_THRESHOLD: u64 = 16 * 1024 * 1024;
const HDD_SEQ_THRESHOLD: u64 = 2 * 1024 * 1024;
const FULL_BUF_SIZE: usize = 1024 * 1024;
const DOUBLE_BUF_THRESHOLD: u64 = 16 * 1024 * 1024;
const DOUBLE_BUF_SIZE: usize = 2 * 1024 * 1024;

fn open_checked_file_with_size(
    path: &Path,
    frn: u64,
    flags: u32,
) -> Option<(File, Option<(u32, u64)>, u64)> {
    if frn == 0 {
        return fastio::open_checked_file_fast(None, 0, path, flags);
    }
    thread_local! {
        static DRIVE_HINTS: RefCell<HashMap<char, Option<File>>> = RefCell::new(HashMap::new());
    }
    let ch = path
        .to_string_lossy()
        .chars()
        .next()
        .unwrap_or('C')
        .to_ascii_uppercase();
    DRIVE_HINTS.with(|cell| {
        let mut hints = cell.borrow_mut();
        let hint = hints
            .entry(ch)
            .or_insert_with(|| fastio::open_drive_hint(ch))
            .as_ref();
        fastio::open_checked_file_fast(hint, frn, path, flags)
    })
}

fn open_checked_file(path: &Path, frn: u64, flags: u32) -> Option<(File, Option<(u32, u64)>)> {
    let (file, file_id, _) = open_checked_file_with_size(path, frn, flags)?;
    Some((file, file_id))
}

fn hash_chunk(
    file: &mut File,
    buf: &mut [u8],
    offset: u64,
    length: u64,
    digest: &mut blake3::Hasher,
) -> Option<()> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut remaining = length;
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let read = file.read(&mut buf[..want]).ok()?;
        if read == 0 {
            break;
        }
        digest.update(&buf[..read]);
        remaining -= read as u64;
    }
    Some(())
}

/// 头部 4KB 哈希（0 次寻道），用于测试或单文件校验。
#[cfg(test)]
fn head_hash(path: &Path, size: u64) -> Option<(hashcache::Digest, Option<(u32, u64)>)> {
    let flags = if size <= 64 * 1024 {
        FILE_FLAG_SEQUENTIAL_SCAN
    } else {
        FILE_FLAG_RANDOM_ACCESS
    };
    let (mut file, file_id) = open_checked_file(path, 0, flags)?;
    let mut buf = [0u8; HEAD_CHUNK as usize];
    let want = HEAD_CHUNK.min(size) as usize;
    let mut filled = 0usize;
    while filled < want {
        let read = file.read(&mut buf[filled..want]).ok()?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    let digest = *blake3::hash(&buf[..filled]).as_bytes();
    Some((digest, file_id))
}

/// 机械盘专用（<= 2MB）：一次顺序读完整文件到内存，同时算出与 SSD 字节对齐的采样哈希与完整哈希，
/// 用一次顺序读代替 3 次跨扇区随机寻道，避免阶段 3 再次打开文件。
fn read_hdd_seq_sample_and_full(
    path: &Path,
    frn: u64,
    size: u64,
    chunk_len: u64,
) -> Option<(hashcache::Digest, hashcache::Digest, Option<(u32, u64)>)> {
    let (mut file, file_id, actual_size) =
        open_checked_file_with_size(path, frn, FILE_FLAG_SEQUENTIAL_SCAN)?;
    if actual_size != size || actual_size > HDD_SEQ_THRESHOLD {
        return None;
    }
    let s = actual_size as usize;
    thread_local! {
        static HDD_SEQ_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; HDD_SEQ_THRESHOLD as usize]);
    }
    HDD_SEQ_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        let mut filled = 0usize;
        while filled < s {
            let read = file.read(&mut buf[filled..s]).ok()?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        if filled != s {
            return None;
        }
        let data = &buf[..s];
        let full = *blake3::hash(data).as_bytes();
        let c = chunk_len as usize;
        let sample = if s <= c * 3 {
            full
        } else {
            let mut h = blake3::Hasher::new();
            h.update(&data[..c]);
            h.update(&data[s / 2..s / 2 + c]);
            h.update(&data[s - c..s]);
            *h.finalize().as_bytes()
        };
        Some((sample, full, file_id))
    })
}

/// 采样哈希：头、中、尾各取 16KB（小文件直接顺序读一次，避免重叠区重复读取）。
fn sample_hash_by_frn(
    path: &Path,
    frn: u64,
    size: u64,
) -> Option<(hashcache::Digest, Option<(u32, u64)>)> {
    if size <= SAMPLE_CHUNK * 3 {
        return full_hash_by_frn(path, frn);
    }
    let (mut file, file_id) = open_checked_file(path, frn, FILE_FLAG_RANDOM_ACCESS)?;
    let mut buf = [0u8; SAMPLE_CHUNK as usize];
    let mut digest = blake3::Hasher::new();
    hash_chunk(&mut file, &mut buf, 0, SAMPLE_CHUNK, &mut digest)?;
    hash_chunk(&mut file, &mut buf, size / 2, SAMPLE_CHUNK, &mut digest)?;
    hash_chunk(
        &mut file,
        &mut buf,
        size - SAMPLE_CHUNK,
        SAMPLE_CHUNK,
        &mut digest,
    )?;
    Some((*digest.finalize().as_bytes(), file_id))
}

#[cfg(test)]
fn sample_hash(path: &Path, size: u64) -> Option<(hashcache::Digest, Option<(u32, u64)>)> {
    sample_hash_by_frn(path, 0, size)
}

/// 强采样（快速模式）：头、中、尾各取 256KB（文件 ≤ 768KB 时直接顺序读全文）。
fn strong_hash_by_frn(
    path: &Path,
    frn: u64,
    size: u64,
) -> Option<(hashcache::Digest, Option<(u32, u64)>)> {
    if size <= STRONG_CHUNK * 3 {
        return full_hash_by_frn(path, frn);
    }
    let (mut file, file_id) = open_checked_file(path, frn, FILE_FLAG_RANDOM_ACCESS)?;
    let mut digest = blake3::Hasher::new();
    thread_local! {
        static STRONG_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; STRONG_CHUNK as usize]);
    }
    let ok = STRONG_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        hash_chunk(&mut file, &mut buf, 0, STRONG_CHUNK, &mut digest)?;
        hash_chunk(&mut file, &mut buf, size / 2, STRONG_CHUNK, &mut digest)?;
        hash_chunk(
            &mut file,
            &mut buf,
            size - STRONG_CHUNK,
            STRONG_CHUNK,
            &mut digest,
        )?;
        Some(())
    });
    ok?;
    Some((*digest.finalize().as_bytes(), file_id))
}

#[cfg(test)]
fn strong_hash(path: &Path, size: u64) -> Option<(hashcache::Digest, Option<(u32, u64)>)> {
    strong_hash_by_frn(path, 0, size)
}

/// 大文件渐进探针：抽取 25% 与 75% 处各 64KB，在全文件读取前提前淘汰中间不同的伪碰撞大文件。
fn probe_hash(
    path: &Path,
    frn: u64,
    size: u64,
) -> Option<(hashcache::Digest, Option<(u32, u64)>)> {
    let (mut file, file_id) = open_checked_file(path, frn, FILE_FLAG_RANDOM_ACCESS)?;
    let mut digest = blake3::Hasher::new();
    thread_local! {
        static PROBE_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; PROBE_CHUNK as usize]);
    }
    let ok = PROBE_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        let off1 = (size / 4).min(size.saturating_sub(PROBE_CHUNK));
        let off2 = (size * 3 / 4).min(size.saturating_sub(PROBE_CHUNK));
        hash_chunk(&mut file, &mut buf, off1, PROBE_CHUNK.min(size), &mut digest)?;
        hash_chunk(&mut file, &mut buf, off2, PROBE_CHUNK.min(size), &mut digest)?;
        Some(())
    });
    ok?;
    Some((*digest.finalize().as_bytes(), file_id))
}

/// 大文件双缓冲顺序校验（>=16MB 时磁盘读与 BLAKE3 计算并行流水线）。
fn full_hash_checked(path: &Path) -> Option<(hashcache::Digest, Option<(u32, u64)>)> {
    full_hash_by_frn(path, 0)
}

fn full_hash_by_frn(path: &Path, frn: u64) -> Option<(hashcache::Digest, Option<(u32, u64)>)> {
    let (mut file, file_id, actual_size) =
        open_checked_file_with_size(path, frn, FILE_FLAG_SEQUENTIAL_SCAN)?;

    if actual_size >= DOUBLE_BUF_THRESHOLD {
        let mut digest = blake3::Hasher::new();
        let (tx_filled, rx_filled) = std::sync::mpsc::sync_channel::<(Vec<u8>, usize)>(1);
        let (tx_empty, rx_empty) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);
        let _ = tx_empty.send(vec![0u8; DOUBLE_BUF_SIZE]);
        let _ = tx_empty.send(vec![0u8; DOUBLE_BUF_SIZE]);

        let ok = std::thread::scope(|scope| {
            let reader = scope.spawn(move || {
                while let Ok(mut buf) = rx_empty.recv() {
                    if DUP_CANCEL.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut filled = 0usize;
                    while filled < buf.len() {
                        match file.read(&mut buf[filled..]) {
                            Ok(0) => break,
                            Ok(n) => filled += n,
                            Err(_) => return false,
                        }
                    }
                    if filled == 0 {
                        break;
                    }
                    if tx_filled.send((buf, filled)).is_err() {
                        break;
                    }
                }
                true
            });

            while let Ok((buf, filled)) = rx_filled.recv() {
                if DUP_CANCEL.load(Ordering::Relaxed) {
                    drop(rx_filled);
                    let _ = reader.join();
                    return false;
                }
                digest.update(&buf[..filled]);
                let _ = tx_empty.send(buf);
            }
            reader.join().unwrap_or(false)
        });
        if !ok || DUP_CANCEL.load(Ordering::Relaxed) {
            return None;
        }
        return Some((*digest.finalize().as_bytes(), file_id));
    }

    let mut digest = blake3::Hasher::new();
    thread_local! {
        static FULL_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; FULL_BUF_SIZE]);
    }
    let ok = FULL_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        let mut chunk_count = 0u32;
        loop {
            if chunk_count & 15 == 0 && DUP_CANCEL.load(Ordering::Relaxed) {
                return None;
            }
            let read = file.read(&mut buf).ok()?;
            if read == 0 {
                break;
            }
            digest.update(&buf[..read]);
            chunk_count = chunk_count.wrapping_add(1);
        }
        Some(())
    });
    ok?;
    Some((*digest.finalize().as_bytes(), file_id))
}

fn full_hash(path: &Path) -> Option<String> {
    full_hash_checked(path).map(|(digest, _)| hashcache::digest_to_hex(&digest))
}

// ---------- 公共目录 ----------

pub(crate) fn cache_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(base).join("C盘清理工具").join("mftcache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 系统命令一律走 `%SystemRoot%\System32` 的绝对路径。
/// 程序以管理员身份运行，按名字调用会让当前目录或 PATH 里的同名程序抢先执行。
pub(crate) fn system32(name: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{}\\System32\\{}", root.trim_end_matches('\\'), name)
}

pub(crate) fn fixed_drives() -> Vec<String> {
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

pub(crate) fn get_drive_free_space(drive: &str) -> Result<(u64, u64), String> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = drive.encode_utf16().chain(std::iter::once(0)).collect();
    let mut free_available: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_free_bytes: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_available,
            &mut total_bytes,
            &mut total_free_bytes,
        )
    };
    if ok != 0 {
        Ok((free_available, total_bytes))
    } else {
        Err("获取磁盘空间失败".to_string())
    }
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
    #[serde(default)]
    keeper: String,
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

#[tauri::command(async)]
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

#[tauri::command(async)]
fn delete_dups(items: Vec<VerifyItem>) -> DeleteReport {
    let title = "重复文件";
    let mut eligible = Vec::new();
    let mut sizes: HashMap<String, u64> = HashMap::new();
    let mut failed = Vec::new();
    let mut reasons = Vec::new();
    let mut pending_verify: Vec<VerifyItem> = Vec::new();
    let mut keeper_set: HashSet<String> = HashSet::new();

    for item in items {
        let path = item.path.clone();
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
        if !item.keeper.is_empty() {
            keeper_set.insert(item.keeper.clone());
        }
        pending_verify.push(item);
    }

    // 并行按盘计算保留项与待删副本的完整哈希（保留“宁可跳过不误删”的逐字节核对）。
    let mut paths_to_hash: Vec<String> = keeper_set.into_iter().collect();
    for item in &pending_verify {
        paths_to_hash.push(item.path.clone());
    }
    let hashed_list = run_disk_aware(
        paths_to_hash,
        |p| p.as_str(),
        |_| 0,
        |p| {
            let h = full_hash(Path::new(&p));
            (p, h)
        },
    );
    let hashed_map: HashMap<String, Option<String>> = hashed_list.into_iter().collect();

    for item in pending_verify {
        let path = item.path;
        let expected = if item.keeper.is_empty() {
            Some(item.hash)
        } else {
            hashed_map.get(&item.keeper).cloned().flatten()
        };
        let Some(expected) = expected else {
            failed.push(path.clone());
            reasons.push(format!("{}：保留的文件读不到，已跳过", path));
            continue;
        };
        match hashed_map.get(&path).and_then(|v| v.as_ref()) {
            Some(hash) if *hash == expected => {
                sizes.insert(path.clone(), item.size);
                eligible.push(path);
            }
            _ => {
                failed.push(path.clone());
                reasons.push(format!("{}：内容与保留文件不一致，已跳过", path));
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

#[tauri::command(async)]
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

#[tauri::command(async)]
fn list_drives() -> Vec<String> {
    fixed_drives()
}

#[tauri::command(async)]
fn frag_analyze(drive: String) -> frag::FragReport {
    frag::analyze(&drive)
}

#[tauri::command(async)]
fn frag_optimize(drive: String) -> Result<(), String> {
    frag::optimize(&drive)
}

// ---------- 命令：微信 / QQ ----------

#[tauri::command(async)]
fn im_roots(kind: String) -> Vec<String> {
    im::roots(&kind)
}

#[tauri::command(async)]
fn im_scan(kind: String) -> Vec<im::ImRecord> {
    im::list_media(&im::roots(&kind))
}

#[tauri::command(async)]
fn im_clean(kind: String, kinds: Vec<String>, paths: Option<Vec<String>>) -> DeleteReport {
    im::clean(&kind, &kinds, paths)
}

// ---------- 命令：隐私清理 ----------

#[tauri::command(async)]
fn privacy_scan() -> Vec<privacy::PrivacyItem> {
    privacy::scan()
}

#[tauri::command(async)]
fn privacy_clean(ids: Vec<String>) -> privacy::PrivacyReport {
    privacy::clean(ids)
}

// ---------- 命令：粉碎 / 清理历史 / 规则更新 ----------

pub(crate) fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[tauri::command(async)]
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

#[tauri::command(async)]
fn history_list() -> Vec<logs::LogEntry> {
    logs::list()
}

#[tauri::command(async)]
fn history_read(name: String) -> Result<String, String> {
    logs::read(&name)
}

#[tauri::command(async)]
fn rules_update() -> Result<String, String> {
    rules::update().map(|(added, total, note)| {
        format!(
            "新增 {} 条，共 {} 条。{}\n下次扫描生效。",
            added, total, note
        )
    })
}

#[tauri::command(async)]
fn rules_rollback() -> Result<String, String> {
    rules::rollback()
}

// ---------- 用户目录 ----------

#[tauri::command(async)]
fn user_profile() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".to_string())
}

// ---------- 命令：新特性支持 (系统级瘦身、微信搬家、资源管理器、进程检测) ----------

#[tauri::command(async)]
fn reveal_path(path: String) -> Result<(), String> {
    system::reveal_in_explorer(&path)
}

#[tauri::command(async)]
fn open_path(path: String) -> Result<(), String> {
    system::open_path(&path)
}

#[tauri::command(async)]
fn check_processes(targets: Vec<String>) -> Vec<String> {
    let refs: Vec<&str> = targets.iter().map(|s| s.as_str()).collect();
    process::check_running(&refs)
}

#[tauri::command(async)]
fn kill_process(name: String) -> Result<usize, String> {
    Ok(process::kill_process_by_name(&name))
}

#[tauri::command(async)]
fn system_scan() -> Vec<system::SystemItem> {
    system::scan_system_deep()
}

#[tauri::command(async)]
fn system_clean(key: String) -> Result<String, String> {
    system::clean_system_item(&key).map(|(_, _, msg)| msg)
}

#[tauri::command(async)]
fn hiber_query() -> system::HiberInfo {
    system::query_hiber()
}

#[tauri::command(async)]
fn hiber_set(mode: String) -> Result<String, String> {
    system::set_hiber(&mode)
}

#[tauri::command(async)]
fn recycle_empty() -> Result<(), String> {
    system::empty_all_recycle_bins()
}

#[tauri::command(async)]
fn im_move_analyze(kind: String) -> system::MoveAnalysis {
    system::analyze_im_migration(&kind)
}

#[tauri::command(async)]
fn im_move_execute(source: String, target_dir: String, kind: String) -> Result<String, String> {
    system::execute_im_migration(&source, &target_dir, &kind)
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

fn is_admin() -> bool {
    // unsafe 块在语句位置会被当成语句解析，包一层括号才可继续写 != 0。
    (unsafe { IsUserAnAdmin() }) != 0
}

/// --index 回填：全盘 MFT 枚举 + 同 size 文件算哈希，填持久化索引库。
/// 计划任务调的就是它。只读不删，不碰任何实时扫描路径。
/// 返回进程退出码：0 全部成功，1 部分卷失败，2 未提权。
/// 单卷索引结果（`--index` / 索引查询的增量追赶共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexVolumeStatus {
    /// 全量回填完成。
    Backfilled,
    /// USN 增量追赶完成。
    Incremented,
    /// 失败（打不开卷 / 元数据写库失败）。
    Failed,
}

struct IndexVolumeOutcome {
    status: IndexVolumeStatus,
    /// 新增/更新的文件行数（回填 = 枚举到的行数；增量 = 变化集里的文件数）。
    files: u64,
    /// 增量：删除的行数（回填恒为 0）。
    deleted: u64,
    /// 增量：目录改名改写的路径行数（回填恒为 0）。
    renamed: u64,
    /// 从 file_hashes 缓存复用的 full 摘要个数。
    reused_full: u64,
    /// 哈希管线新算的 full 摘要个数。
    hashed_new: u64,
    elapsed_secs: f64,
}

/// 索引进度日志：`logs/index-YYYYMMDD.log`（与扫描日志同目录）。
/// 只写给人看的进度行；写失败静默忽略（不影响索引本身）。
fn index_log(msg: &str) {
    let name = format!("index-{}.log", &timeutil::stamp()[..8]);
    logs::append_line(&name, msg);
}

/// 增量维护的哈希补算：对"同 size ≥ 2 且组内至少一个缺 full"的组跑哈希管线。
/// 摘要来源优先级：索引行已有 > file_hashes 缓存 > 管线新算；新摘要双写（索引 + 缓存）。
/// 返回（从缓存复用的 full 个数，管线新算的 full 个数）。
/// 注意：索引行没有 MFT 布局信息（first_lcn/can_raw_read），这里的候选一律走普通文件读，
/// 不走 raw-read 快车道；增量组通常很小，影响可忽略（首次回填走全量路径，有完整布局信息）。
/// 分流与管线执行与 scan_dups/回填共用（`split_dup_candidates` / `run_dup_pipelines`）；
/// 索引行转候选的摘要来源不同，转换留在这里。
fn hash_index_groups(index: &mut fileindex::FileIndex, serial: u64, stamp_now: u64) -> (u64, u64) {
    let rows = index.rows_needing_full(serial);
    if rows.is_empty() {
        return (0, 0);
    }
    let mut cache = hashcache::HashCache::load_for_paths(rows.iter().map(|r| r.path.as_str()));
    let mut by_size: HashMap<u64, Vec<DupCandidate>> = HashMap::new();
    let mut disk_cache: HashMap<char, DiskKey> = HashMap::new();
    let mut reused: HashSet<(u64, u64)> = HashSet::new();
    let mut had_full: HashSet<(u64, u64)> = HashSet::new();
    // 索引行转候选：摘要来源是索引行已有 > file_hashes 缓存（与 FileEntry 源不同，
    // 转换留在这里）；布局信息没有，走普通文件读（不走 raw-read 快车道），见设计文档 §3。
    for r in &rows {
        let is_ssd = disk_key_for_path(&r.path, &mut disk_cache) == DiskKey::Ssd;
        let cached = cache.entry_for(&r.path, r.size, r.mtime);
        let head = r.head.or_else(|| cached.and_then(|e| e.head));
        let sample = r.sample.or_else(|| cached.and_then(|e| e.sample));
        let strong = r.strong.or_else(|| cached.and_then(|e| e.strong));
        let full = r.full.or_else(|| cached.and_then(|e| e.full));
        if full.is_some() {
            had_full.insert((serial, r.frn));
            if r.full.is_none() {
                // 索引里没有、缓存里有：复用。
                reused.insert((serial, r.frn));
            }
        }
        by_size.entry(r.size).or_default().push(DupCandidate {
            path: r.path.clone(),
            size: r.size,
            mtime: r.mtime,
            sort_key: 0,
            frn: r.frn,
            first_lcn: 0,
            bytes_per_cluster: 0,
            can_raw_read: false,
            is_ssd,
            file_id: None,
            head,
            sample,
            strong,
            full,
            probe: None,
            dirty_head: false,
            dirty_sample: false,
            dirty_strong: false,
            dirty_full: false,
        });
    }
    let (ready_final, ssd_only_stage1, mixed_stage1) = split_dup_candidates(by_size, false);
    let (final_items, eliminated_items) = run_dup_pipelines(
        ssd_only_stage1,
        mixed_stage1,
        &mut cache,
        stamp_now,
        false,
        false,
        "index-ssd",
        "index-hdd",
        false,
    );
    let mut hashed_items = final_items;
    hashed_items.extend(eliminated_items);
    let mut updates: Vec<fileindex::DigestUpdate> = Vec::new();
    for item in ready_final.values().flatten().chain(hashed_items.iter()) {
        if item.head.is_some()
            || item.sample.is_some()
            || item.strong.is_some()
            || item.full.is_some()
        {
            updates.push(fileindex::DigestUpdate {
                volume_serial: serial,
                frn: item.frn,
                size: item.size,
                mtime: item.mtime,
                head: item.head,
                sample: item.sample,
                strong: item.strong,
                full: item.full,
            });
        }
    }
    index.set_digests_batch(&updates);
    let reused_full = reused.len() as u64;
    let hashed_new = hashed_items
        .iter()
        .filter(|c| c.full.is_some() && !had_full.contains(&(serial, c.frn)))
        .count() as u64;
    (reused_full, hashed_new)
}

/// 增量维护：把一个 USN 变化集应用到索引（删行 / 路径前缀改写 / 元数据 upsert），
/// 再对缺失摘要的组做哈希补算。游标在元数据落库后、哈希之前写——中断后续跑
/// 只补缺失摘要，不重扫。
fn index_volume_incremental(
    index: &mut fileindex::FileIndex,
    drive: &str,
    serial: u64,
    changes: mft::UsnChangeSet,
    quiet: bool,
    do_hash: bool,
    started: std::time::Instant,
) -> IndexVolumeOutcome {
    let stamp_now = now_seconds();
    let deleted = index.delete_rows(serial, &changes.deleted_frns) as u64;
    let mut renamed = 0u64;
    for (old, new) in &changes.renamed_dirs {
        renamed += index.update_path_prefix(serial, old, new) as u64;
    }
    let rows: Vec<fileindex::IndexRow> = changes
        .files
        .iter()
        .map(|f| fileindex::IndexRow {
            volume_serial: serial,
            frn: f.frn,
            path: f.path.clone(),
            size: f.size,
            mtime: f.mtime,
            last_seen: stamp_now,
        })
        .collect();
    let files = rows.len() as u64;
    if !rows.is_empty() && index.upsert_files(&rows) == 0 {
        // 事务失败：游标不写，下次重新追赶。
        let msg = format!("--index：{} 增量元数据写入失败。", drive);
        eprintln!("{}", msg);
        index_log(&msg);
        return IndexVolumeOutcome {
            status: IndexVolumeStatus::Failed,
            files: 0,
            deleted,
            renamed,
            reused_full: 0,
            hashed_new: 0,
            elapsed_secs: started.elapsed().as_secs_f64(),
        };
    }
    index.set_usn_state(serial, changes.journal_id, changes.next_usn);
    let (reused_full, hashed_new) = if do_hash {
        hash_index_groups(index, serial, stamp_now)
    } else {
        (0, 0)
    };
    let elapsed = started.elapsed().as_secs_f64();
    let line = format!(
        "index: {} 增量：删 {} 行，路径修正 {} 个，新增/更新 {} 个文件，复用缓存 {} 个，新算 {} 个，用时 {:.1}s",
        drive, deleted, renamed, files, reused_full, hashed_new, elapsed
    );
    if !quiet {
        println!("{}", line);
    }
    index_log(&line);
    IndexVolumeOutcome {
        status: IndexVolumeStatus::Incremented,
        files,
        deleted,
        renamed,
        reused_full,
        hashed_new,
        elapsed_secs: elapsed,
    }
}

/// 单卷索引入口：游标可用时走 USN 增量追赶，否则全量回填。
/// `do_hash` 只影响增量路径（big 页的纯元数据追赶用 false；回填总是算摘要——
/// 索引缺失/断档时 big 页回落实时扫描，不走无摘要回填）。
/// `rebuild` 为真时先清掉该卷的索引行和游标，再全量回填。
fn index_volume(
    index: &mut fileindex::FileIndex,
    drive: &str,
    quiet: bool,
    do_hash: bool,
    rebuild: bool,
) -> IndexVolumeOutcome {
    let started = std::time::Instant::now();
    let Some((serial, journal_id, first_usn, next_usn)) = mft::volume_usn_state(drive) else {
        let msg = format!("--index：{} 打不开卷或 USN 日志。", drive);
        eprintln!("{}", msg);
        index_log(&msg);
        return IndexVolumeOutcome {
            status: IndexVolumeStatus::Failed,
            files: 0,
            deleted: 0,
            renamed: 0,
            reused_full: 0,
            hashed_new: 0,
            elapsed_secs: started.elapsed().as_secs_f64(),
        };
    };
    if rebuild {
        index.delete_volume(serial);
    }
    if !rebuild {
        if let Some((stored_journal_id, stored_next_usn)) = index.get_usn_state(serial) {
            if fileindex::usn_state_usable(
                stored_journal_id,
                stored_next_usn,
                journal_id,
                first_usn,
                next_usn,
            ) {
                if let Some(changes) =
                    mft::usn_changes_since(drive, stored_journal_id, stored_next_usn, &TOP_SKIP)
                {
                    return index_volume_incremental(
                        index, drive, serial, changes, quiet, do_hash, started,
                    );
                }
                // 变化集取不到（无快照 / 变化太多 / 日志读失败）：回落全量回填。
                if !quiet {
                    println!("index: {} 增量不可用（变化集取不到），全量回填", drive);
                }
            }
        }
    }
    index_volume_backfill(index, drive, serial, journal_id, next_usn, quiet)
}

/// 单卷增量追赶（仅增量，不回填）：游标可用且变化集可取时返回 Some(结果)，否则 None。
/// 查询路径（dup/big 页）用这个——索引缺失/断档时页面回落实时扫描，不触发全量回填；
/// `--index` 用 `index_volume`（断档时回填）。
fn index_volume_incremental_only(
    index: &mut fileindex::FileIndex,
    drive: &str,
    quiet: bool,
    do_hash: bool,
) -> Option<IndexVolumeOutcome> {
    let started = std::time::Instant::now();
    let Some((serial, journal_id, first_usn, next_usn)) = mft::volume_usn_state(drive) else {
        return None;
    };
    let Some((stored_journal_id, stored_next_usn)) = index.get_usn_state(serial) else {
        return None;
    };
    if !fileindex::usn_state_usable(
        stored_journal_id,
        stored_next_usn,
        journal_id,
        first_usn,
        next_usn,
    ) {
        return None;
    }
    let changes = mft::usn_changes_since(drive, stored_journal_id, stored_next_usn, &TOP_SKIP)?;
    Some(index_volume_incremental(
        index, drive, serial, changes, quiet, do_hash, started,
    ))
}

/// 全量回填一卷：MFT 枚举 → 元数据 upsert → 游标写入 → 摘要复用/哈希管线 → 摘要写回。
/// 游标在元数据落库后、哈希之前写（断点续跑：中断后续跑走增量，只补缺失摘要）。
fn index_volume_backfill(
    index: &mut fileindex::FileIndex,
    drive: &str,
    serial: u64,
    journal_id: u64,
    next_usn: i64,
    quiet: bool,
) -> IndexVolumeOutcome {
    let started = std::time::Instant::now();
    let stamp_now = now_seconds();
    let entries = mft::scan_files(drive, fileindex::INDEX_MIN_SIZE, &TOP_SKIP);
    // 先算好每个条目的 mtime（MFT 里没有时读文件属性，与 scan_dups 一致）；
    // 遍历回退的条目没有 FRN，进不了索引（身份是 (卷序列号, FRN)）。
    let total_entries = entries.len();
    let mut resolved: Vec<(mft::FileEntry, u64)> = Vec::with_capacity(total_entries);
    for e in entries {
        if e.frn == 0 {
            continue;
        }
        let mtime = if e.mtime != 0 {
            e.mtime
        } else {
            file_mtime(&e.path)
        };
        resolved.push((e, mtime));
    }
    let skipped = total_entries - resolved.len();
    let rows: Vec<fileindex::IndexRow> = resolved
        .iter()
        .map(|(e, mtime)| fileindex::IndexRow {
            volume_serial: serial,
            frn: e.frn,
            path: e.path.clone(),
            size: e.size,
            mtime: *mtime,
            last_seen: stamp_now,
        })
        .collect();
    index.upsert_files(&rows);
    // 游标先写（元数据之后、哈希之前）：中断后续跑走增量，只补缺失摘要，不重扫。
    index.set_usn_state(serial, journal_id, next_usn);

    // 复用摘要缓存：已有 (path,size,mtime) 有效摘要的直接拷进索引。
    let mut cache =
        hashcache::HashCache::load_for_paths(resolved.iter().map(|(e, _)| e.path.as_str()));
    let mut reused: HashSet<(u64, u64)> = HashSet::new();
    let mut reuse_updates: Vec<fileindex::DigestUpdate> = Vec::new();
    for (e, mtime) in &resolved {
        if let Some(entry) = cache.entry_for(&e.path, e.size, *mtime) {
            if entry.full.is_some() {
                reused.insert((serial, e.frn));
            }
            if entry.head.is_some()
                || entry.sample.is_some()
                || entry.strong.is_some()
                || entry.full.is_some()
            {
                reuse_updates.push(fileindex::DigestUpdate {
                    volume_serial: serial,
                    frn: e.frn,
                    size: e.size,
                    mtime: *mtime,
                    head: entry.head,
                    sample: entry.sample,
                    strong: entry.strong,
                    full: entry.full,
                });
            }
        }
    }
    let reused_full = reused.len() as u64;
    index.set_digests_batch(&reuse_updates);

    // 按 size 分组，只有 ≥2 个文件同 size 的组才进哈希管线（大小唯一的零 I/O 排除，
    // 与 scan_dups 同一条规则）。
    let mut by_size: HashMap<u64, Vec<DupCandidate>> = HashMap::new();
    let mut disk_cache: HashMap<char, DiskKey> = HashMap::new();
    for (entry, mtime) in resolved {
        let size = entry.size;
        let candidate = dup_candidate_from_file_entry(entry, mtime, &cache, &mut disk_cache);
        by_size.entry(size).or_default().push(candidate);
    }
    let (ready_final, ssd_only_stage1, mixed_stage1) = split_dup_candidates(by_size, false);

    let (final_items, eliminated_items) = run_dup_pipelines(
        ssd_only_stage1,
        mixed_stage1,
        &mut cache,
        stamp_now,
        false,
        false,
        "index-ssd",
        "index-hdd",
        false,
    );
    let mut hashed_items = final_items;
    hashed_items.extend(eliminated_items);

    // 管线算出的摘要（以及 ready 组里已有的）写回索引，按 (size, mtime) 校验。
    let mut updates: Vec<fileindex::DigestUpdate> = Vec::new();
    for item in ready_final.values().flatten().chain(hashed_items.iter()) {
        if item.head.is_some()
            || item.sample.is_some()
            || item.strong.is_some()
            || item.full.is_some()
        {
            updates.push(fileindex::DigestUpdate {
                volume_serial: serial,
                frn: item.frn,
                size: item.size,
                mtime: item.mtime,
                head: item.head,
                sample: item.sample,
                strong: item.strong,
                full: item.full,
            });
        }
    }
    index.set_digests_batch(&updates);
    let newly = hashed_items
        .iter()
        .filter(|c| c.full.is_some() && !reused.contains(&(serial, c.frn)))
        .count() as u64;
    let elapsed = started.elapsed().as_secs_f64();
    let line = format!(
        "index: {} 文件 {} 个（跳过无 FRN {} 个），复用缓存 {} 个，新算 {} 个，用时 {:.1}s",
        drive,
        rows.len(),
        skipped,
        reused_full,
        newly,
        elapsed
    );
    if !quiet {
        println!("{}", line);
    }
    index_log(&line);
    IndexVolumeOutcome {
        status: IndexVolumeStatus::Backfilled,
        files: rows.len() as u64,
        deleted: 0,
        renamed: 0,
        reused_full,
        hashed_new: newly,
        elapsed_secs: elapsed,
    }
}

/// `--index` 入口：逐卷索引（增量优先，断档回填），`--rebuild` 强制全量重建。
/// 退出码：0 全部成功，1 部分卷失败，2 非管理员。
fn run_index(quiet: bool, rebuild: bool) -> i32 {
    if !is_admin() {
        eprintln!("--index 需要管理员权限：请用管理员身份运行（计划任务请用最高权限）。");
        return 2;
    }
    let drives = fixed_drives();
    if drives.is_empty() {
        eprintln!("--index：没有找到固定磁盘。");
        return 1;
    }
    let mut index = match fileindex::FileIndex::open_default() {
        Some(index) => index,
        None => {
            eprintln!("--index：打不开索引库。");
            return 1;
        }
    };
    let (summary, ok) = index_all_drives(&mut index, &drives, quiet, rebuild);
    if !quiet {
        println!("{}", summary);
    }
    index_log(&summary);
    if ok {
        0
    } else {
        1
    }
}

// ---------- 索引查询（Phase 3） ----------

/// 索引新鲜度（查询前每卷一次 QUERY_USN_JOURNAL）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexFreshness {
    /// 游标与当前一致，直接查。
    Fresh,
    /// 落后但在日志内，先增量追赶再查。
    Behind,
    /// 断档（日志重建 / 游标超出范围），回落实时扫描。
    Gap,
    /// 该卷没有索引（无游标），回落实时扫描。
    Missing,
    /// 卷打不开（非管理员 / 盘符没了），回落实时扫描。
    Unavailable,
}

/// 由已取到的卷状态算新鲜度（`volume_usn_state` 的返回值直接传进来，避免重复开卷）。
fn freshness_from_state(
    index: &fileindex::FileIndex,
    serial: u64,
    journal_id: u64,
    first_usn: i64,
    next_usn: i64,
) -> IndexFreshness {
    let Some((stored_journal_id, stored_next_usn)) = index.get_usn_state(serial) else {
        return IndexFreshness::Missing;
    };
    if !fileindex::usn_state_usable(
        stored_journal_id,
        stored_next_usn,
        journal_id,
        first_usn,
        next_usn,
    ) {
        return IndexFreshness::Gap;
    }
    if stored_next_usn == next_usn {
        IndexFreshness::Fresh
    } else {
        IndexFreshness::Behind
    }
}

/// 查询前确保一卷新鲜：Fresh 直接用；Behind 跑一次增量（仅增量，不回填）；
/// 其余返回原值（调用方回落实时扫描）。
/// `do_hash`：重复页 true（需要摘要），大文件页 false（只要元数据）。
/// 返回（新鲜度，卷序列号）。
fn ensure_index_scope_fresh(
    index: &mut fileindex::FileIndex,
    drive: &str,
    do_hash: bool,
) -> (IndexFreshness, Option<u64>) {
    let Some((serial, journal_id, first_usn, next_usn)) = mft::volume_usn_state(drive) else {
        return (IndexFreshness::Unavailable, None);
    };
    let freshness = freshness_from_state(index, serial, journal_id, first_usn, next_usn);
    if freshness != IndexFreshness::Behind {
        return (freshness, Some(serial));
    }
    match index_volume_incremental_only(index, drive, true, do_hash) {
        Some(outcome) => match outcome.status {
            IndexVolumeStatus::Incremented | IndexVolumeStatus::Backfilled => {
                (IndexFreshness::Fresh, Some(serial))
            }
            IndexVolumeStatus::Failed => (IndexFreshness::Gap, Some(serial)),
        },
        None => (IndexFreshness::Gap, Some(serial)),
    }
}

/// 解析 scan_dups/scan_big 的一个 root：返回（盘符 "C:\"，子目录前缀或 None）。
/// 盘符根（"C:\"/"C:"）→ 整卷；子目录（用户目录）→ 该卷 + 前缀。
fn parse_index_scope_root(root: &str) -> Option<(String, Option<String>)> {
    let trimmed = root.trim_end_matches('\\');
    let letter = trimmed.chars().next()?;
    if trimmed.len() < 2 || trimmed.as_bytes()[1] != b':' || !letter.is_ascii_alphabetic() {
        return None;
    }
    let drive = format!("{}:\\", letter.to_ascii_uppercase());
    let prefix = if trimmed.len() > 2 {
        Some(trimmed.to_string())
    } else {
        None
    };
    Some((drive, prefix))
}

/// `path` 是否在目录 `dir_prefix` 之下（不含目录本身；边界必须是 `\`）。
/// 大小写不敏感（Windows 路径），按字节边界安全切片（中文目录名不 panic）。
fn path_under_dir(path: &str, dir_prefix: &str) -> bool {
    let Some(head) = path.get(..dir_prefix.len()) else {
        return false;
    };
    head.eq_ignore_ascii_case(dir_prefix)
        && path
            .as_bytes()
            .get(dir_prefix.len())
            .map(|b| *b == b'\\')
            .unwrap_or(false)
}

/// 重复页查索引：返回 Some(组) 表示索引可用且完整；None 表示回落实时扫描。
/// 完整性保证：任一 size 组有成员缺所需摘要（fast→strong，否则 full）即回落，
/// 索引路径永不漏报。系统文件（pagefile.sys 等）与实时扫描一致，排除。
fn query_dups_from_index(
    index: &mut fileindex::FileIndex,
    roots: &[String],
    min_size: u64,
    fast: bool,
) -> Option<Vec<DupGroup>> {
    if min_size < fileindex::INDEX_MIN_SIZE {
        return None;
    }
    let mut scopes: Vec<(u64, Option<String>)> = Vec::new();
    for root in roots {
        let (drive, prefix) = parse_index_scope_root(root)?;
        let (freshness, serial) = ensure_index_scope_fresh(index, &drive, true);
        if freshness != IndexFreshness::Fresh {
            return None;
        }
        let Some(serial) = serial else {
            return None;
        };
        scopes.push((serial, prefix));
    }
    if scopes.is_empty() {
        return None;
    }
    let serials: Vec<u64> = scopes.iter().map(|(s, _)| *s).collect();
    let rows = index.query_dup_rows(&serials, min_size);
    // 子目录作用域：只留该卷下前缀内的行；系统文件排除。
    let rows: Vec<fileindex::IndexDupRow> = rows
        .into_iter()
        .filter(|r| !is_system_file_name(&r.path))
        .filter(|r| {
            scopes.iter().any(|(serial, prefix)| {
                *serial == r.volume_serial
                    && match prefix {
                        None => true,
                        Some(prefix) => path_under_dir(&r.path, prefix),
                    }
            })
        })
        .collect();
    // 按 size 分组：组内任一成员缺所需摘要 → 不完整，回落实时扫描。
    let mut by_size: HashMap<u64, Vec<fileindex::IndexDupRow>> = HashMap::new();
    for r in rows {
        by_size.entry(r.size).or_default().push(r);
    }
    let mut by_hash: HashMap<(u64, hashcache::Digest), Vec<DupCandidate>> = HashMap::new();
    for bucket in by_size.into_values() {
        if bucket.len() < 2 {
            continue;
        }
        let digest_of = |r: &fileindex::IndexDupRow| if fast { r.strong } else { r.full };
        if bucket.iter().any(|r| digest_of(r).is_none()) {
            return None;
        }
        for r in bucket {
            let digest = digest_of(&r).unwrap();
            // 索引里一 FRN 一行，硬链接天然去重；file_id 直接填 (卷序列号低 32 位, FRN)，
            // 让 build_dup_groups 跳过逐文件的 query_file_identity。
            by_hash
                .entry((r.size, digest))
                .or_default()
                .push(DupCandidate {
                    path: r.path,
                    size: r.size,
                    mtime: r.mtime,
                    sort_key: 0,
                    frn: r.frn,
                    first_lcn: 0,
                    bytes_per_cluster: 0,
                    can_raw_read: false,
                    is_ssd: false,
                    file_id: Some((r.volume_serial as u32, r.frn)),
                    head: r.head,
                    sample: r.sample,
                    strong: r.strong,
                    full: r.full,
                    probe: None,
                    dirty_head: false,
                    dirty_sample: false,
                    dirty_strong: false,
                    dirty_full: false,
                });
        }
    }
    let mut groups = build_dup_groups(by_hash);
    sort_dup_groups(&mut groups);
    Some(groups)
}

/// 大文件页查索引：返回 Some(行) 表示索引可用；None 表示回落实时扫描。
/// 只追赶元数据（do_hash=false，不哈希）；索引缺失/断档时不回填，直接回落。
fn query_big_from_index(
    index: &mut fileindex::FileIndex,
    drives: &[String],
) -> Option<Vec<BigFile>> {
    let mut serials: Vec<u64> = Vec::new();
    for drive in drives {
        let (freshness, serial) = ensure_index_scope_fresh(index, drive, false);
        if freshness != IndexFreshness::Fresh {
            return None;
        }
        let Some(serial) = serial else {
            return None;
        };
        serials.push(serial);
    }
    if serials.is_empty() {
        return None;
    }
    let rows = index.query_big_rows(&serials);
    let mut out: Vec<BigFile> = rows
        .into_iter()
        .filter(|r| !is_system_file_name(&r.path))
        .filter(|r| r.size >= big_file_threshold_bytes(&r.path))
        .map(|r| BigFile {
            path: r.path,
            size: r.size,
            mtime: r.mtime,
        })
        .collect();
    out.sort_by(|a, b| b.size.cmp(&a.size));
    Some(out)
}

/// 一卷的索引状态（UI 用）。
#[derive(Serialize)]
struct IndexVolumeState {
    drive: String,
    /// fresh | behind | stale | missing | unavailable
    state: String,
    files: u64,
    with_full: u64,
    /// USN 游标更新时间（unix 秒），没有游标时 0。
    updated_at: u64,
}

#[derive(Serialize)]
struct IndexState {
    volumes: Vec<IndexVolumeState>,
}

/// UI 的索引状态：逐卷新鲜度 + 行数 + 游标时间。只读，不触发同步。
#[tauri::command(async)]
fn index_status() -> IndexState {
    let mut volumes = Vec::new();
    let index = fileindex::FileIndex::open_default();
    for drive in fixed_drives() {
        let (state, files, with_full, updated_at) = match &index {
            Some(idx) => match mft::volume_usn_state(&drive) {
                Some((serial, journal_id, first_usn, next_usn)) => {
                    let (files, with_full) = idx.volume_counts(serial);
                    let updated_at = idx.usn_state_updated_at(serial).unwrap_or(0);
                    let state = match idx.get_usn_state(serial) {
                        None => "missing",
                        Some((stored_journal_id, stored_next_usn)) => {
                            if !fileindex::usn_state_usable(
                                stored_journal_id,
                                stored_next_usn,
                                journal_id,
                                first_usn,
                                next_usn,
                            ) {
                                "stale"
                            } else if stored_next_usn == next_usn {
                                "fresh"
                            } else {
                                "behind"
                            }
                        }
                    };
                    (state.to_string(), files, with_full, updated_at)
                }
                None => ("unavailable".to_string(), 0, 0, 0),
            },
            None => ("missing".to_string(), 0, 0, 0),
        };
        volumes.push(IndexVolumeState {
            drive,
            state,
            files,
            with_full,
            updated_at,
        });
    }
    IndexState { volumes }
}

/// 逐卷索引并返回汇总行（`--index` 与 UI 的"立即同步"/"重建索引"共用）。
/// 返回（汇总行，是否全部成功）。
fn index_all_drives(
    index: &mut fileindex::FileIndex,
    drives: &[String],
    quiet: bool,
    rebuild: bool,
) -> (String, bool) {
    let mut failed: Vec<String> = Vec::new();
    let mut total_files: u64 = 0;
    let mut total_hashed: u64 = 0;
    let mut total_reused: u64 = 0;
    let mut total_incremented: u64 = 0;
    let mut total_deleted: u64 = 0;
    let mut total_renamed: u64 = 0;
    let mut total_elapsed: f64 = 0.0;
    for drive in drives {
        let outcome = index_volume(index, drive, quiet, true, rebuild);
        total_files += outcome.files;
        total_hashed += outcome.hashed_new;
        total_reused += outcome.reused_full;
        total_deleted += outcome.deleted;
        total_renamed += outcome.renamed;
        total_elapsed += outcome.elapsed_secs;
        if outcome.status == IndexVolumeStatus::Incremented {
            total_incremented += 1;
        }
        if outcome.status == IndexVolumeStatus::Failed {
            failed.push(drive.clone());
        }
    }
    let (files, with_full) = index.counts();
    let summary = format!(
        "index: 本次 {} 个文件（其中 {} 卷增量，删 {} 行，路径修正 {} 个），复用缓存 {} 个，新算 {} 个，用时 {:.1}s；库中共 {} 个文件（{} 个有 full 摘要）{}",
        total_files,
        total_incremented,
        total_deleted,
        total_renamed,
        total_reused,
        total_hashed,
        total_elapsed,
        files,
        with_full,
        if failed.is_empty() {
            String::new()
        } else {
            format!("；失败的卷：{}", failed.join("、"))
        }
    );
    (summary, failed.is_empty())
}

/// UI 的"立即同步"：逐卷增量追赶（断档回填），返回汇总行。耗时操作，前端会等它完成。
#[tauri::command(async)]
fn index_sync_now() -> String {
    if !is_admin() {
        return "--index 需要管理员权限。".to_string();
    }
    let drives = fixed_drives();
    let mut index = match fileindex::FileIndex::open_default() {
        Some(index) => index,
        None => return "--index：打不开索引库。".to_string(),
    };
    let (summary, _) = index_all_drives(&mut index, &drives, true, false);
    index_log(&summary);
    summary
}

/// UI 的"重建索引"：清掉全部索引行和游标，全量重建。耗时操作，前端会等它完成。
#[tauri::command(async)]
fn index_rebuild() -> String {
    if !is_admin() {
        return "--index 需要管理员权限。".to_string();
    }
    let drives = fixed_drives();
    let mut index = match fileindex::FileIndex::open_default() {
        Some(index) => index,
        None => return "--index：打不开索引库。".to_string(),
    };
    let (summary, _) = index_all_drives(&mut index, &drives, true, true);
    index_log(&summary);
    summary
}

#[derive(Serialize)]
struct IndexTaskState {
    exists: bool,
    enabled: bool,
}

/// UI 的计划任务状态：是否已注册、是否启用。只读，不触发注册。
#[tauri::command(async)]
fn index_task_status() -> IndexTaskState {
    let (exists, enabled) = indextask::task_status();
    IndexTaskState { exists, enabled }
}

/// UI 的"运行计划任务"：手动跑一次 `\CDriveCleaner\Index`（后台执行）。
/// 这也是 S4U 提权的验证入口——跑完读索引日志确认走 MFT 路径且 exit 0。
#[tauri::command(async)]
fn index_task_run() -> String {
    match indextask::run_task() {
        Ok(msg) => {
            index_log(&format!("计划任务：{}", msg));
            msg
        }
        Err(e) => e,
    }
}

/// UI 的启用 / 禁用计划任务。
#[tauri::command(async)]
fn index_task_set_enabled(enabled: bool) -> String {
    match indextask::set_task_enabled(enabled) {
        Ok(msg) => {
            index_log(&format!("计划任务：{}", msg));
            msg
        }
        Err(e) => e,
    }
}

/// UI 的手动重注册（启动时已提权的前提下已在后台幂等注册过一次）。
#[tauri::command(async)]
fn index_task_register() -> String {
    match indextask::register_task() {
        Ok(msg) => {
            index_log(&format!("计划任务：{}", msg));
            msg
        }
        Err(e) => e,
    }
}

/// 删除计划任务（卸载流程用；UI 不提供按钮）。
#[tauri::command(async)]
fn index_task_delete() -> String {
    match indextask::delete_task() {
        Ok(msg) => {
            index_log(&format!("计划任务：{}", msg));
            msg
        }
        Err(e) => e,
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
    let dups = scan_dups(Some("C:\\".to_string()), None, None, None);
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
    // --index 在建任何窗口之前分支出命令行模式：计划任务在非交互会话里触发它，
    // 不能起 WebView，也不能弹 UAC（未提权时 run_index 直接报错退出）。
    if std::env::args().any(|arg| arg == "--index") {
        let quiet = std::env::args().any(|arg| arg == "--quiet");
        let rebuild = std::env::args().any(|arg| arg == "--rebuild");
        std::process::exit(run_index(quiet, rebuild));
    }
    ensure_admin();
    // 已提权的前提下，后台幂等重注册每日 03:00 的索引计划任务（`/f` 覆盖）。
    // 不阻塞启动：注册失败只记索引日志。非提权时 ensure_admin 已重走 UAC，
    // 能走到这里的一定是提权进程。
    std::thread::spawn(|| match indextask::register_task() {
        Ok(msg) => index_log(&format!("计划任务：{}", msg)),
        Err(e) => index_log(&format!("计划任务注册失败：{}", e)),
    });
    // 上次搬家如果停在"改名"和"建联接"之间，先把数据目录还原回去。
    system::recover_pending_move();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            disk_info,
            scan_status,
            cancel_scan,
            scan_categories,
            scan_big,
            scan_dups,
            index_status,
            index_sync_now,
            index_rebuild,
            index_task_status,
            index_task_run,
            index_task_set_enabled,
            index_task_register,
            index_task_delete,
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
            privacy_clean,
            reveal_path,
            open_path,
            check_processes,
            kill_process,
            system_scan,
            system_clean,
            hiber_query,
            hiber_set,
            recycle_empty,
            im_move_analyze,
            im_move_execute
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
        let keeper = dir.join("keep.bin");
        let first = dir.join("a.bin");
        let second = dir.join("b.bin");
        std::fs::write(&keeper, b"same-content").unwrap();
        std::fs::write(&first, b"same-content").unwrap();
        std::fs::write(&second, b"dame-content").unwrap();
        let stats = |path: &std::path::Path| {
            let meta = std::fs::metadata(path).unwrap();
            let mtime = meta
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            (meta.len(), mtime)
        };
        let (size, mtime) = stats(&first);
        let (size2, mtime2) = stats(&second);
        let keeper_str = keeper.to_string_lossy().to_string();

        let good = VerifyItem {
            path: first.to_string_lossy().to_string(),
            hash: full_hash(&first).unwrap(),
            size,
            mtime,
            keeper: keeper_str.clone(),
        };
        // 内容与保留项不一致的条目必须保留。
        let bad = VerifyItem {
            path: second.to_string_lossy().to_string(),
            hash: full_hash(&second).unwrap(),
            size: size2,
            mtime: mtime2,
            keeper: keeper_str,
        };
        let report = delete_dups(vec![good, bad]);
        assert_eq!(report.deleted, 1);
        assert!(!first.exists());
        assert!(second.exists());
        assert!(keeper.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cascading_hashes_distinguish_head_middle_and_tail_differences() {
        let dir = sandbox("cdc-cascade-hash-test");
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        let c = dir.join("c.bin");

        // 1MB+ files sharing identical 4KB head and tail, differing only in the middle.
        let size = 1024 * 1024;
        let buf_a = vec![0xAAu8; size];
        let mut buf_b = vec![0xAAu8; size];
        let mut buf_c = vec![0xBBu8; size];
        buf_b[size / 2] = 0x55;
        buf_c[size - 16..] .copy_from_slice(&[0xAAu8; 16]);

        std::fs::write(&a, &buf_a).unwrap();
        std::fs::write(&b, &buf_b).unwrap();
        std::fs::write(&c, &buf_c).unwrap();

        let ha = head_hash(&a, size as u64).unwrap().0;
        let hb = head_hash(&b, size as u64).unwrap().0;
        let hc = head_hash(&c, size as u64).unwrap().0;
        assert_eq!(ha, hb);
        assert_ne!(ha, hc);

        let sa = sample_hash(&a, size as u64).unwrap().0;
        let sb = sample_hash(&b, size as u64).unwrap().0;
        assert_ne!(sa, sb);

        let sta = strong_hash(&a, size as u64).unwrap().0;
        let stb = strong_hash(&b, size as u64).unwrap().0;
        assert_ne!(sta, stb);

        // Small file <= HEAD_CHUNK matches across all hash tiers.
        let small = dir.join("small.bin");
        std::fs::write(&small, b"tiny-data").unwrap();
        let h_small = head_hash(&small, 9).unwrap().0;
        let s_small = sample_hash(&small, 9).unwrap().0;
        let st_small = strong_hash(&small, 9).unwrap().0;
        let f_small = full_hash_checked(&small).unwrap().0;
        assert_eq!(h_small, s_small);
        assert_eq!(s_small, st_small);
        assert_eq!(st_small, f_small);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hardlinks_are_deduplicated_in_dup_groups() {
        let dir = sandbox("cdc-hardlink-test");
        let orig = dir.join("orig.bin");
        let link = dir.join("link.bin");
        let copy = dir.join("copy.bin");
        let data = vec![0x5Au8; 8192];
        std::fs::write(&orig, &data).unwrap();
        std::fs::write(&copy, &data).unwrap();
        if std::fs::hard_link(&orig, &link).is_ok() {
            let (d1, id1) = head_hash(&orig, data.len() as u64).unwrap();
            let (d2, id2) = head_hash(&link, data.len() as u64).unwrap();
            let (d3, id3) = head_hash(&copy, data.len() as u64).unwrap();
            assert_eq!(d1, d2);
            assert_eq!(d1, d3);
            assert!(id1.is_some());
            assert_eq!(id1, id2);
            assert_ne!(id1, id3);

            let mut by_hash: HashMap<(u64, hashcache::Digest), Vec<DupCandidate>> = HashMap::new();
            by_hash.insert(
                (data.len() as u64, d1),
                vec![
                    DupCandidate {
                        path: orig.to_string_lossy().to_string(),
                        size: data.len() as u64,
                        mtime: 1,
                        sort_key: 0,
                        frn: 0,
                        first_lcn: 0,
                        bytes_per_cluster: 0,
                        can_raw_read: false,
                        is_ssd: true,
                        file_id: id1,
                        head: Some(d1),
                        sample: None,
                        strong: None,
                        full: Some(d1),
                        probe: None,
                        dirty_head: false,
                        dirty_sample: false,
                        dirty_strong: false,
                        dirty_full: false,
                    },
                    DupCandidate {
                        path: link.to_string_lossy().to_string(),
                        size: data.len() as u64,
                        mtime: 1,
                        sort_key: 0,
                        frn: 0,
                        first_lcn: 0,
                        bytes_per_cluster: 0,
                        can_raw_read: false,
                        is_ssd: true,
                        file_id: id2,
                        head: Some(d2),
                        sample: None,
                        strong: None,
                        full: Some(d2),
                        probe: None,
                        dirty_head: false,
                        dirty_sample: false,
                        dirty_strong: false,
                        dirty_full: false,
                    },
                    DupCandidate {
                        path: copy.to_string_lossy().to_string(),
                        size: data.len() as u64,
                        mtime: 1,
                        sort_key: 0,
                        frn: 0,
                        first_lcn: 0,
                        bytes_per_cluster: 0,
                        can_raw_read: false,
                        is_ssd: true,
                        file_id: id3,
                        head: Some(d3),
                        sample: None,
                        strong: None,
                        full: Some(d3),
                        probe: None,
                        dirty_head: false,
                        dirty_sample: false,
                        dirty_strong: false,
                        dirty_full: false,
                    },
                ],
            );
            let groups = build_dup_groups(by_hash);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].files.len(), 2);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hdd_sequential_read_matches_ssd_sample_and_strong_hashes() {
        let dir = sandbox("cdc-hdd-seq-test");
        let file = dir.join("1_5mb.bin");
        let size = 1536 * 1024;
        let data: Vec<u8> = (0..size).map(|i| ((i * 31 + 7) % 251) as u8).collect();
        std::fs::write(&file, &data).unwrap();

        let (hdd_sample, hdd_full, _) =
            read_hdd_seq_sample_and_full(&file, 0, size as u64, SAMPLE_CHUNK).unwrap();
        let (ssd_sample, _) = sample_hash(&file, size as u64).unwrap();
        let (expected_full, _) = full_hash_checked(&file).unwrap();
        assert_eq!(hdd_sample, ssd_sample);
        assert_eq!(hdd_full, expected_full);

        let (hdd_strong, hdd_full2, _) =
            read_hdd_seq_sample_and_full(&file, 0, size as u64, STRONG_CHUNK).unwrap();
        let (ssd_strong, _) = strong_hash(&file, size as u64).unwrap();
        assert_eq!(hdd_strong, ssd_strong);
        assert_eq!(hdd_full2, expected_full);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_hash_catches_quarter_and_three_quarter_differences() {
        let dir = sandbox("cdc-probe-test");
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        let size = 1024 * 1024;
        let data_a = vec![0x11u8; size];
        let mut data_b = vec![0x11u8; size];
        data_b[size / 4 + 10] = 0x99;

        std::fs::write(&a, &data_a).unwrap();
        std::fs::write(&b, &data_b).unwrap();

        // 头、中、尾 16KB 全部碰撞，但 25% 处探针直接区分开
        assert_eq!(sample_hash(&a, size as u64).unwrap().0, sample_hash(&b, size as u64).unwrap().0);
        assert_ne!(probe_hash(&a, 0, size as u64).unwrap().0, probe_hash(&b, 0, size as u64).unwrap().0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn warm_cache_hardlinks_with_none_file_id_still_deduplicate() {
        let dir = sandbox("cdc-warm-hardlink-test");
        let orig = dir.join("orig.bin");
        let link = dir.join("link.bin");
        let copy = dir.join("copy.bin");
        let data = vec![0x3Cu8; 8192];
        std::fs::write(&orig, &data).unwrap();
        std::fs::write(&copy, &data).unwrap();
        if std::fs::hard_link(&orig, &link).is_ok() {
            let digest = *blake3::hash(&data).as_bytes();
            let make = |p: &std::path::Path| DupCandidate {
                path: p.to_string_lossy().to_string(),
                size: data.len() as u64,
                mtime: 1,
                sort_key: 0,
                frn: 0,
                first_lcn: 0,
                bytes_per_cluster: 0,
                can_raw_read: false,
                is_ssd: true,
                file_id: None,
                head: Some(digest),
                sample: Some(digest),
                strong: Some(digest),
                full: Some(digest),
                probe: None,
                dirty_head: false,
                dirty_sample: false,
                dirty_strong: false,
                dirty_full: false,
            };
            let mut by_hash = HashMap::new();
            by_hash.insert((data.len() as u64, digest), vec![make(&orig), make(&link), make(&copy)]);
            let groups = build_dup_groups(by_hash);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].files.len(), 2);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn big_file_thresholds_follow_kbigfile_tiers() {
        const MB: u64 = 1024 * 1024;
        assert_eq!(big_file_threshold_bytes("C:\\a\\setup.exe"), 100 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\setup.msi"), 100 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\movie.mp4"), 20 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\movie.MP4"), 20 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\clip.tmp"), 20 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\song.mp3"), 5 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\pack.zip"), 5 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\app.apk"), 5 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\photo.jpg"), 2 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\doc.pdf"), 2 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\app.log"), 2 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\data.bin"), 50 * MB);
        assert_eq!(big_file_threshold_bytes("C:\\a\\README"), 50 * MB);
    }
}
