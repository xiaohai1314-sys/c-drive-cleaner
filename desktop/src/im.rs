//! 微信 / QQ 专清：只识别媒体、缓存和日志，聊天数据库与配置一律跳过。
//! 对应 Python 版 imclean.py。

use serde::Serialize;
use std::path::Path;

use crate::recycle;
use crate::timeutil;

const VIDEO_EXT: [&str; 15] = [
    ".mp4", ".avi", ".mov", ".mkv", ".wmv", ".flv", ".m4v", ".3gp", ".mpeg", ".mpg", ".ts",
    ".webm", ".rmvb", ".vob", ".m2ts",
];
const IMAGE_EXT: [&str; 14] = [
    ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".heic", ".tif", ".tiff", ".ico", ".raw",
    ".psd", ".svg", ".jfif",
];
const AUDIO_EXT: [&str; 8] = [
    ".silk", ".amr", ".mp3", ".m4a", ".wav", ".ogg", ".aac", ".aud",
];
const DOC_EXT: [&str; 18] = [
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".txt", ".csv", ".rtf", ".ofd",
    ".md", ".wps", ".et", ".dps", ".dwg", ".dxf", ".xmind",
];
const ARCHIVE_EXT: [&str; 9] = [
    ".zip", ".rar", ".7z", ".tar", ".gz", ".bz2", ".xz", ".iso", ".cab",
];
const FILE_EXT: [&str; 2] = [".psd", ".ai"];
const SKIP_EXT: [&str; 14] = [
    ".db", ".db-wal", ".db-shm", ".sqlite", ".sqlite3", ".ini", ".key", ".xml", ".json", ".pfx",
    ".crt", ".exe", ".dll", ".cfg",
];
const SKIP_DIR_PARTS: [&str; 8] = [
    "db_storage",
    "config",
    "account",
    "apputil",
    "login",
    "db",
    "database",
    "databases",
];

const MEDIA_DIRS: [(&str, &[&str]); 7] = [
    ("sns", &["sns", "moments", "timeline"]),
    ("video", &["video", "videos"]),
    (
        "image",
        &[
            "image",
            "images",
            "img",
            "attach",
            "msgattach",
            "custom_emotion",
            "emotion",
        ],
    ),
    ("audio", &["voice", "audio", "ptt", "sound"]),
    (
        "file",
        &["file", "files", "filerecv", "file_recv", "fav", "favorite"],
    ),
    ("log", &["log", "logs", "xlog", "crash", "crashinfo"]),
    ("cache", &["cache", "temp", "emoji", "emoticon", "cdncomm"]),
];

#[derive(Serialize, Clone)]
pub struct ImRecord {
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub kind: String,
    pub sub: String,
    pub month: String,
    pub name: String,
}

fn env_path(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

fn wechat_registry_path_value() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new(crate::system32("reg.exe"))
        .args([
            "query",
            "HKCU\\Software\\Tencent\\WeChat",
            "/v",
            "FileSavePath",
        ])
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("FileSavePath") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let path = parts[2..].join(" ");
                if path != "MyDocument:" {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn wechat_registry_path() -> Option<String> {
    let path = wechat_registry_path_value()?;
    let p = Path::new(&path);
    if p.is_dir() {
        return Some(path);
    }
    let sub = p.join("WeChat Files");
    if sub.is_dir() {
        return Some(sub.to_string_lossy().to_string());
    }
    None
}

fn wechat_ini_path_value() -> Option<String> {
    let roaming = env_path("APPDATA");
    let ini = Path::new(&roaming).join("Tencent\\WeChat\\All Users\\config\\3ebffe94.ini");
    if let Ok(content) = std::fs::read_to_string(&ini) {
        for line in content.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim().eq_ignore_ascii_case("FileSavePath") {
                    let val = v.trim();
                    if val != "MyDocument:" {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }
    None
}

fn wechat_ini_path() -> Option<String> {
    let path = wechat_ini_path_value()?;
    let p = Path::new(&path);
    if p.is_dir() {
        return Some(path);
    }
    let sub = p.join("WeChat Files");
    if sub.is_dir() {
        return Some(sub.to_string_lossy().to_string());
    }
    None
}

fn default_root_candidates(kind: &str) -> Vec<String> {
    let roaming = env_path("APPDATA");
    let profile = env_path("USERPROFILE");
    if kind == "wechat" {
        vec![
            format!("{}\\Tencent\\WeChat", roaming),
            format!("{}\\Tencent\\xwechat", roaming),
            format!("{}\\Documents\\WeChat Files", profile),
            format!("{}\\Documents\\xwechat_files", profile),
        ]
    } else {
        vec![
            format!("{}\\Tencent\\QQ", roaming),
            format!("{}\\Tencent\\QQNT", roaming),
            format!("{}\\Documents\\Tencent Files", profile),
        ]
    }
}

fn append_wechat_migration_candidates(candidates: &mut Vec<String>, value: &str) {
    candidates.push(value.to_string());
    candidates.push(
        Path::new(value)
            .join("WeChat Files")
            .to_string_lossy()
            .to_string(),
    );
}

/// 返回迁移源白名单候选，不要求当前目录存在：恢复时源目录刚被改名，正是不存在的状态。
pub(crate) fn migration_roots(kind: &str) -> Vec<String> {
    let mut candidates = default_root_candidates(kind);
    if kind == "wechat" {
        for value in [wechat_registry_path_value(), wechat_ini_path_value()]
            .into_iter()
            .flatten()
        {
            append_wechat_migration_candidates(&mut candidates, &value);
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

pub fn roots(kind: &str) -> Vec<String> {
    let mut candidates = default_root_candidates(kind);
    if kind == "wechat" {
        if let Some(custom) = wechat_registry_path() {
            candidates.push(custom);
        }
        if let Some(custom) = wechat_ini_path() {
            candidates.push(custom);
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .filter(|path| Path::new(path).is_dir())
        .collect()
}

fn parts_of(path: &str) -> Vec<String> {
    path.replace('/', "\\")
        .to_lowercase()
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

fn relative_parts(path: &str, root: Option<&str>) -> Vec<String> {
    let full = parts_of(path);
    if let Some(root) = root {
        let base = parts_of(root);
        if full.len() > base.len() && full[..base.len()] == base[..] {
            return full[base.len()..].to_vec();
        }
    }
    full
}

fn ext_of(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or("");
    match name.rfind('.') {
        Some(index) => name[index..].to_lowercase(),
        None => String::new(),
    }
}

fn dir_kind(dirs: &[String]) -> Option<&'static str> {
    for (kind, names) in MEDIA_DIRS {
        if dirs.iter().any(|dir| names.contains(&dir.as_str())) {
            return Some(kind);
        }
    }
    None
}

fn received_subkind(path: &str) -> &'static str {
    let ext = ext_of(path);
    let ext = ext.as_str();
    if DOC_EXT.contains(&ext) {
        return "doc";
    }
    if VIDEO_EXT.contains(&ext) {
        return "video";
    }
    if IMAGE_EXT.contains(&ext) {
        return "image";
    }
    if AUDIO_EXT.contains(&ext) {
        return "music";
    }
    if ARCHIVE_EXT.contains(&ext) {
        return "archive";
    }
    "other"
}

pub fn classify(path: &str, root: Option<&str>) -> Option<(String, String)> {
    let parts = relative_parts(path, root);
    if parts.is_empty() {
        return None;
    }
    let ext = ext_of(&parts[parts.len() - 1]);
    if SKIP_EXT.contains(&ext.as_str()) {
        return None;
    }
    let dirs = &parts[..parts.len() - 1];
    if dirs
        .iter()
        .any(|dir| SKIP_DIR_PARTS.contains(&dir.as_str()))
    {
        return None;
    }
    let kind = if let Some(dir_kind) = dir_kind(dirs) {
        dir_kind.to_string()
    } else if ext.is_empty() || ext == ".dat" || ext == ".tmp" {
        return None;
    } else if VIDEO_EXT.contains(&ext.as_str()) {
        "video".to_string()
    } else if IMAGE_EXT.contains(&ext.as_str()) {
        "image".to_string()
    } else if AUDIO_EXT.contains(&ext.as_str()) {
        "audio".to_string()
    } else if FILE_EXT.contains(&ext.as_str())
        || ARCHIVE_EXT.contains(&ext.as_str())
        || DOC_EXT.contains(&ext.as_str())
    {
        "file".to_string()
    } else if matches!(ext.as_str(), ".log" | ".xlog" | ".dmp") {
        "log".to_string()
    } else {
        return None;
    };
    let sub = if kind == "file" {
        received_subkind(path).to_string()
    } else {
        String::new()
    };
    Some((kind, sub))
}

fn local_month(seconds: u64) -> String {
    timeutil::month_label(seconds)
}

pub fn list_media(roots: &[String]) -> Vec<ImRecord> {
    let mut found = Vec::new();
    for root in roots {
        if !Path::new(root).is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|item| item.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().to_string_lossy().to_string();
            let Ok(meta) = entry.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let Some((kind, sub)) = classify(&path, Some(root)) else {
                continue;
            };
            let name = Path::new(&path)
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
            found.push(ImRecord {
                path,
                size: meta.len(),
                mtime,
                kind,
                sub,
                month: local_month(mtime),
                name,
            });
        }
    }
    found.sort_by(|left, right| right.mtime.cmp(&left.mtime));
    found
}

pub fn clean(kind: &str, kinds: &[String], paths: Option<Vec<String>>) -> crate::DeleteReport {
    let roots = roots(kind);
    let mut chosen: Vec<ImRecord> = Vec::new();
    match paths {
        None => {
            for record in list_media(&roots) {
                if kinds.contains(&record.kind) {
                    chosen.push(record);
                }
            }
        }
        Some(paths) => {
            for path in paths {
                if path.trim().is_empty() || !crate::safety::is_within(&path, &roots) {
                    continue;
                }
                if crate::safety::is_password_store(&path)
                    || crate::safety::is_protected_path(&path)
                {
                    continue;
                }
                let root = roots
                    .iter()
                    .filter(|root| crate::safety::is_within(&path, &[root.to_string()]))
                    .max_by_key(|root| root.len())
                    .cloned();
                let Some((record_kind, sub)) = classify(&path, root.as_deref()) else {
                    continue;
                };
                if !kinds.contains(&record_kind) {
                    continue;
                }
                let Ok(meta) = std::fs::metadata(&path) else {
                    continue;
                };
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);
                chosen.push(ImRecord {
                    name: Path::new(&path)
                        .file_name()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    path,
                    size: meta.len(),
                    mtime,
                    kind: record_kind,
                    sub,
                    month: local_month(mtime),
                });
            }
        }
    }
    if chosen.is_empty() {
        return crate::DeleteReport {
            freed: 0,
            deleted: 0,
            failed: Vec::new(),
            reasons: Vec::new(),
        };
    }
    let batch: Vec<String> = chosen.iter().map(|record| record.path.clone()).collect();
    let _ = recycle::send_to_recycle(&batch);
    let mut freed = 0u64;
    let mut deleted = 0u64;
    let mut failed = Vec::new();
    let mut reasons = Vec::new();
    for record in &chosen {
        if Path::new(&record.path).exists() {
            failed.push(record.path.clone());
            reasons.push(format!("{}：送回收站失败（可能被占用）", record.path));
        } else {
            freed += record.size;
            deleted += 1;
        }
    }
    crate::invalidate_dup_cache();
    let _ = crate::logs::write_log(&[
        "开始清理".to_string(),
        format!("清理：{}", if kind == "wechat" { "微信" } else { "QQ" }),
        format!("完成：移到回收站 {} 个，释放 {} 字节", deleted, freed),
    ]);
    crate::DeleteReport {
        freed,
        deleted,
        failed,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_roots_include_default_candidates_without_checking_existence() {
        let expected = format!("{}\\Documents\\Tencent Files", env_path("USERPROFILE"));
        assert!(migration_roots("qq").contains(&expected));
    }

    #[test]
    fn custom_wechat_migration_candidates_survive_missing_source_directory() {
        let custom = format!(
            "C:\\migration-root-that-does-not-exist-{}",
            std::process::id()
        );
        let mut candidates = Vec::new();
        append_wechat_migration_candidates(&mut candidates, &custom);
        assert_eq!(candidates[0], custom);
        assert_eq!(
            candidates[1],
            Path::new(&custom)
                .join("WeChat Files")
                .to_string_lossy()
                .to_string()
        );
    }

    #[test]
    fn classifies_by_directory_before_extension() {
        let root = "C:\\Users\\u\\Documents\\WeChat Files";
        // 接收文件目录里的视频仍算「接收文件」。
        let (kind, _sub) = classify(
            "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\FileStorage\\File\\2024-01\\a.mp4",
            Some(root),
        )
        .unwrap();
        assert_eq!(kind, "file");
        // 缓存目录里的图片算缓存。
        let (kind, _sub) = classify(
            "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\FileStorage\\Cache\\a.jpg",
            Some(root),
        )
        .unwrap();
        assert_eq!(kind, "cache");
    }

    #[test]
    fn skips_databases_and_config() {
        let root = "C:\\Users\\u\\Documents\\WeChat Files";
        assert!(classify(
            "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\Msg\\a.db",
            Some(root)
        )
        .is_none());
        assert!(classify(
            "C:\\Users\\u\\Documents\\WeChat Files\\wxid\\db_storage\\a.dat",
            Some(root)
        )
        .is_none());
    }

    #[test]
    fn classifies_plain_media_without_directory_hint() {
        let (kind, sub) = classify(
            "C:\\Users\\u\\Documents\\WeChat Files\\x\\a.pdf",
            Some("C:\\Users\\u\\Documents\\WeChat Files"),
        )
        .unwrap();
        assert_eq!(kind, "file");
        assert_eq!(sub, "doc");
        assert!(classify(
            "C:\\Users\\u\\Documents\\WeChat Files\\x\\a.unknownext",
            Some("C:\\Users\\u\\Documents\\WeChat Files")
        )
        .is_none());
    }
}
