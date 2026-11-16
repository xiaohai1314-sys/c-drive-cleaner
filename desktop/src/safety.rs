//! 路径安全与删除，对应 Python 版 safepath.py 的核心判断。
//!
//! 原则：宁可跳过，不可误删。所有删除前都过一遍永不清理名单、受保护路径、
//! 用户文档库、密码库和重解析点检查；删除时按句柄操作，避免父目录在检查后
//! 被换成链接。

use glob::Pattern;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::OnceLock;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const DELETE_ACCESS: u32 = 0x0001_0000;
const FILE_DISPOSITION_INFO_CLASS: i32 = 4;

const PASSWORD_FILENAMES: [&str; 8] = [
    "login data",
    "login data-journal",
    "login data for account",
    "login data for account-journal",
    "logins.json",
    "logins-backup.json",
    "key4.db",
    "key3.db",
];

const CRITICAL_FILES: [&str; 7] = [
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "bootmgr",
    "ntldr",
    "ntuser.dat",
    "ntuser.dat.log",
];

const NEVER_CLEAN_ROOTS: [&str; 2] = ["\\recovery", "\\system volume information"];
const NEVER_CLEAN_FILES: [&str; 1] = ["usmt.ppkg"];

const LIBRARY_NAMES: [&str; 10] = [
    "documents", "desktop", "downloads", "my documents", "文档",
    "桌面", "下载", "pictures", "music", "videos",
];

#[repr(C)]
struct FileDispositionInfo {
    delete_file: u8,
}

#[link(name = "kernel32")]
extern "system" {
    fn SetFileInformationByHandle(
        handle: *mut core::ffi::c_void,
        class: i32,
        info: *mut core::ffi::c_void,
        size: u32,
    ) -> i32;
    fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
}

pub enum DeleteOutcome {
    Deleted,
    Skipped,
    Pending,
    Failed(String),
}

pub fn basename(path: &str) -> String {
    let text = path.replace('/', "\\");
    let text = text.trim_end_matches('\\');
    text.rsplit('\\').next().unwrap_or("").to_string()
}

pub fn normalize(path: &str) -> String {
    let text = path.trim().trim_matches('"').replace('/', "\\");
    if text.len() >= 2 && text.as_bytes()[1] == b':' {
        let drive = text[..2].to_uppercase();
        let mut rest = text[2..].to_string();
        while rest.contains("\\\\") {
            rest = rest.replace("\\\\", "\\");
        }
        if rest.is_empty() || rest == "\\" {
            return format!("{}\\", drive);
        }
        if !rest.starts_with('\\') {
            rest.insert(0, '\\');
        }
        return format!("{}{}", drive, rest).trim_end_matches('\\').to_string();
    }
    text.trim_end_matches(['\\', '/']).to_string()
}

fn tail(path: &str) -> String {
    let norm = normalize(path);
    let rest = if norm.len() >= 2 && norm.as_bytes()[1] == b':' {
        &norm[2..]
    } else {
        &norm[..]
    };
    let mut rest = rest.replace('/', "\\").to_lowercase();
    if !rest.starts_with('\\') {
        rest.insert(0, '\\');
    }
    rest
}

fn is_under(tail_text: &str, marker: &str) -> bool {
    let marker = if marker.starts_with('\\') {
        marker.to_lowercase()
    } else {
        format!("\\{}", marker.to_lowercase())
    };
    tail_text == marker || tail_text.starts_with(&format!("{}\\", marker))
}

pub fn under_marker(path: &str, markers: &[&str]) -> bool {
    let text = tail(path);
    markers.iter().any(|marker| is_under(&text, marker))
}

pub fn is_drive_root(path: &str) -> bool {
    let norm = normalize(path);
    (norm.len() == 2 && norm.as_bytes()[1] == b':')
        || (norm.len() == 3 && norm.as_bytes()[1] == b':' && (norm.ends_with('\\') || norm.ends_with('/')))
}

pub fn is_critical_file(path: &str) -> bool {
    let name = basename(path).to_lowercase();
    CRITICAL_FILES.iter().any(|item| *item == name)
}

pub fn is_never_clean(path: &str) -> bool {
    if path.trim().is_empty() {
        return true;
    }
    let name = basename(path).to_lowercase();
    if NEVER_CLEAN_FILES.iter().any(|item| *item == name) {
        return true;
    }
    under_marker(path, &NEVER_CLEAN_ROOTS)
}

pub fn is_password_store(path: &str) -> bool {
    let name = basename(path).to_lowercase();
    PASSWORD_FILENAMES.iter().any(|item| *item == name)
}

pub fn path_syntax_error(path: &str) -> bool {
    let text = path.trim();
    if text.is_empty() {
        return true;
    }
    if text.starts_with("\\\\") || text.starts_with("//") {
        return true;
    }
    let body = if text.len() >= 2 && text.as_bytes()[1] == b':' {
        &text[2..]
    } else {
        text
    };
    if body.contains(':') {
        return true;
    }
    text.split(['\\', '/']).any(|part| part == "..")
}

fn env_roots() -> Vec<String> {
    let mut roots = Vec::new();
    for name in ["SystemRoot", "WINDIR", "ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
        if let Ok(value) = std::env::var(name) {
            roots.push(normalize(&value).to_lowercase());
        }
    }
    roots
}

pub fn is_system_path(path: &str) -> bool {
    if path.trim().is_empty() || is_drive_root(path) || is_critical_file(path) {
        return true;
    }
    if under_marker(
        path,
        &["\\windows", "\\system volume information", "\\recovery", "\\boot", "\\$recycle.bin"],
    ) {
        return true;
    }
    let target = normalize(path).to_lowercase().trim_end_matches(['\\', '/']).to_string();
    for key in ["SystemRoot", "WINDIR"] {
        if let Ok(value) = std::env::var(key) {
            let root = normalize(&value).to_lowercase().trim_end_matches(['\\', '/']).to_string();
            if !root.is_empty() && (target == root || target.starts_with(&format!("{}\\", root))) {
                return true;
            }
        }
    }
    false
}

pub fn is_protected_path(path: &str) -> bool {
    if is_system_path(path) {
        return true;
    }
    if under_marker(
        path,
        &["\\program files", "\\program files (x86)", "\\programdata", "\\perflogs"],
    ) {
        return true;
    }
    let target = normalize(path).to_lowercase().trim_end_matches(['\\', '/']).to_string();
    for root in env_roots() {
        let root = root.trim_end_matches(['\\', '/']);
        if !root.is_empty() && (target == root || target.starts_with(&format!("{}\\", root))) {
            return true;
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if target == normalize(&profile).to_lowercase().trim_end_matches(['\\', '/']) {
            return true;
        }
    }
    false
}

pub fn user_library_roots() -> Vec<String> {
    let mut bases = Vec::new();
    for key in ["USERPROFILE", "HOME", "OneDrive", "OneDriveConsumer"] {
        if let Ok(value) = std::env::var(key) {
            bases.push(value);
        }
    }
    let names = ["Documents", "Desktop", "Downloads", "My Documents", "文档",
                 "桌面", "下载", "Pictures", "Music", "Videos"];
    let mut roots = Vec::new();
    for base in bases {
        for name in names {
            roots.push(normalize(&format!("{}\\{}", base.trim_end_matches('\\'), name)).to_lowercase());
        }
    }
    roots
}

pub fn is_user_library(path: &str) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    let target = normalize(path).to_lowercase().trim_end_matches('\\').to_string();
    for root in user_library_roots() {
        let root = root.trim_end_matches('\\');
        if target == root || target.starts_with(&format!("{}\\", root)) {
            return true;
        }
    }
    let text = tail(path);
    let parts: Vec<&str> = text.trim_start_matches('\\').split('\\').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 3 && parts[0] == "users" && LIBRARY_NAMES.contains(&parts[2]) {
        return true;
    }
    if parts.len() >= 2 && parts[0] == "users" && LIBRARY_NAMES.contains(&parts[1]) {
        return true;
    }
    false
}

pub fn is_within(path: &str, roots: &[String]) -> bool {
    if roots.is_empty() {
        return false;
    }
    let target = normalize(path).to_lowercase().trim_end_matches(['\\', '/']).to_string();
    for root in roots {
        let base = normalize(root).to_lowercase().trim_end_matches(['\\', '/']).to_string();
        if base.is_empty() {
            continue;
        }
        if target == base || target.starts_with(&format!("{}\\", base)) {
            return true;
        }
    }
    false
}

pub fn is_reparse_point(path: &str) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || meta.file_type().is_symlink()
}

fn parents_are_plain(path: &str, allowed: &[String]) -> bool {
    if !is_within(path, allowed) {
        return false;
    }
    let mut current = normalize(path);
    let mut guard = 0;
    while !current.is_empty() && guard < 128 {
        guard += 1;
        if is_reparse_point(&current) {
            return false;
        }
        let parent = match Path::new(&current).parent() {
            Some(parent) => parent.to_string_lossy().to_string(),
            None => break,
        };
        if parent.is_empty() || parent == current {
            break;
        }
        current = parent;
    }
    true
}

// ---------- AppData 白名单（与 rules.json 分离的 catalog） ----------

type Catalog = HashMap<String, HashMap<String, Vec<Vec<Pattern>>>>;

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut out: Catalog = HashMap::new();
        out.insert("localappdata".to_string(), HashMap::new());
        out.insert("appdata".to_string(), HashMap::new());
        let raw = include_str!("../../appdata_path_allowlist.json");
        let Ok(data) = serde_json::from_str::<serde_json::Value>(raw) else {
            return out;
        };
        let Some(paths) = data.get("paths").and_then(|value| value.as_array()) else {
            return out;
        };
        for item in paths {
            let Some(text) = item.as_str() else { continue };
            let folded = text.replace('/', "\\");
            let lower = folded.to_lowercase();
            for key in ["localappdata", "appdata"] {
                let marker = format!("%{}%\\", key);
                if !lower.starts_with(&marker) {
                    continue;
                }
                let relative = &folded[marker.len()..];
                let parts: Vec<&str> = relative.split('\\').filter(|p| !p.is_empty()).collect();
                if parts.is_empty() || parts[0].chars().any(|c| matches!(c, '*' | '?' | '[')) {
                    continue;
                }
                let patterns: Vec<Pattern> = parts[1..]
                    .iter()
                    .filter_map(|part| Pattern::new(&part.to_lowercase()).ok())
                    .collect();
                out.get_mut(key)
                    .unwrap()
                    .entry(parts[0].to_lowercase())
                    .or_default()
                    .push(patterns);
                break;
            }
        }
        out
    })
}

fn catalog_matches(parts: &[String], key: &str) -> bool {
    if parts.is_empty() {
        return false;
    }
    let Some(map) = catalog().get(key) else {
        return false;
    };
    let Some(patterns) = map.get(&parts[0].to_lowercase()) else {
        return false;
    };
    for pattern in patterns {
        if parts.len() < pattern.len() + 1 {
            continue;
        }
        if pattern
            .iter()
            .zip(parts[1..].iter())
            .all(|(expected, value)| expected.matches(&value.to_lowercase()))
        {
            return true;
        }
    }
    false
}

fn split_parts(relative: &str) -> Vec<String> {
    relative
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

fn known_appdata_target(path: &str) -> bool {
    let raw = path.trim().replace('/', "\\");
    let lower = raw.to_lowercase();
    for key in ["localappdata", "appdata"] {
        let marker = format!("%{}%\\", key);
        if lower.starts_with(&marker) {
            return catalog_matches(&split_parts(&raw[marker.len()..]), key);
        }
    }
    for (key, env_name) in [("localappdata", "LOCALAPPDATA"), ("appdata", "APPDATA")] {
        let Ok(root) = std::env::var(env_name) else {
            continue;
        };
        let root = normalize(&root).to_lowercase();
        let target = normalize(&raw).to_lowercase();
        if !(target == root || target.starts_with(&format!("{}\\", root))) {
            continue;
        }
        let relative = target[root.len()..].trim_start_matches(['\\', '/']).to_string();
        if catalog_matches(&split_parts(&relative), key) {
            return true;
        }
    }
    false
}

pub fn under_remote_allowlist(path: &str) -> bool {
    if path_syntax_error(path) {
        return false;
    }
    let normalized = normalize(path);
    if is_system_path(&normalized) || is_user_library(&normalized) {
        return false;
    }
    let mut roots = Vec::new();
    for key in ["TEMP", "TMP"] {
        if let Ok(value) = std::env::var(key) {
            if !path_syntax_error(&value) {
                roots.push(value);
            }
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        roots.push(format!("{}\\Temp", local.trim_end_matches('\\')));
    }
    if is_within(&normalized, &roots) {
        return true;
    }
    known_appdata_target(&normalized)
}

pub fn execution_forbidden(path: &str, untrusted: bool) -> bool {
    if path_syntax_error(path) {
        return true;
    }
    if is_never_clean(path) {
        return true;
    }
    if is_reparse_point(path) || Path::new(path).is_symlink() {
        return true;
    }
    if is_password_store(path) || is_user_library(path) {
        return true;
    }
    if is_drive_root(path) || is_critical_file(path) {
        return true;
    }
    if untrusted && (is_system_path(path) || !under_remote_allowlist(path)) {
        return true;
    }
    false
}

/// 按句柄删除普通文件：打开时带 OPEN_REPARSE_POINT，确认不是链接和目录后
/// 设置删除标记。失败时先去掉只读属性重试，仍失败则登记重启后删除。
pub fn delete_file_checked(path: &str, roots: &[String], untrusted: bool) -> DeleteOutcome {
    if execution_forbidden(path, untrusted) {
        return DeleteOutcome::Skipped;
    }
    if !parents_are_plain(path, roots) {
        return DeleteOutcome::Skipped;
    }
    match delete_by_handle(path) {
        Ok(true) => DeleteOutcome::Deleted,
        Ok(false) => match clear_readonly_and_retry(path) {
            true => DeleteOutcome::Deleted,
            false => match schedule_delete(path) {
                true => DeleteOutcome::Pending,
                false => DeleteOutcome::Failed(format!("无法删除：{}", path)),
            },
        },
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                DeleteOutcome::Skipped
            } else {
                match schedule_delete(path) {
                    true => DeleteOutcome::Pending,
                    false => DeleteOutcome::Failed(format!("{}：{}", path, error)),
                }
            }
        }
    }
}

fn delete_by_handle(path: &str) -> std::io::Result<bool> {
    let file = OpenOptions::new()
        .access_mode(DELETE_ACCESS)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let meta = file.metadata()?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(false);
    }
    if meta.is_dir() {
        return Ok(false);
    }
    Ok(mark_delete(&file))
}

/// 在已打开的句柄上设置删除标记（句柄关闭时生效）。粉碎功能复用这个。
pub fn mark_delete(file: &std::fs::File) -> bool {
    let mut info = FileDispositionInfo { delete_file: 1 };
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as *mut core::ffi::c_void,
            FILE_DISPOSITION_INFO_CLASS,
            &mut info as *mut _ as *mut core::ffi::c_void,
            1,
        )
    };
    ok != 0
}

fn clear_readonly_and_retry(path: &str) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let mut permissions = meta.permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        if std::fs::set_permissions(path, permissions).is_err() {
            return false;
        }
    } else {
        return false;
    }
    matches!(delete_by_handle(path), Ok(true))
}

pub fn schedule_delete(path: &str) -> bool {
    let existing: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { MoveFileExW(existing.as_ptr(), std::ptr::null(), 0x4) != 0 }
}

/// 自底向上删除空目录（不删 root 本身），never_clean 命中的目录保留。
pub fn remove_empty_dirs(root: &str) -> usize {
    let mut dirs = Vec::new();
    let mut stack = vec![normalize(root)];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let path = entry.path().to_string_lossy().to_string();
            dirs.push(path.clone());
            stack.push(path);
        }
    }
    dirs.sort_by_key(|path| std::cmp::Reverse(path.len()));
    let mut removed = 0;
    for path in dirs {
        if normalize(&path) == normalize(root) || is_never_clean(&path) {
            continue;
        }
        if std::fs::remove_dir(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keeps_windows_shape() {
        assert_eq!(normalize("C:/Windows/Temp/"), "C:\\Windows\\Temp");
        assert_eq!(normalize("c:\\Windows\\\\Temp"), "C:\\Windows\\Temp");
        assert_eq!(normalize("C:\\"), "C:\\");
    }

    #[test]
    fn protected_paths_are_forbidden() {
        // 内置项（trusted）可以清 Windows\Temp，所以系统路径只在不可信规则下拦；
        // 规则文件里的外置规则一律按不可信处理。
        assert!(is_system_path("C:\\Windows\\System32\\kernel32.dll"));
        assert!(execution_forbidden("C:\\Windows\\System32\\kernel32.dll", true));
        assert!(!execution_forbidden("C:\\Windows\\Temp\\a.tmp", false));
        assert!(execution_forbidden("C:\\pagefile.sys", false));
        assert!(execution_forbidden("C:\\Program Files\\App\\a.dll", true));
        assert!(execution_forbidden("C:\\$Recycle.Bin\\x", true));
        let documents = format!(
            "{}\\Documents\\note.txt",
            std::env::var("USERPROFILE").unwrap_or_default()
        );
        assert!(execution_forbidden(&documents, false));
    }

    #[test]
    fn password_store_is_forbidden() {
        assert!(is_password_store("C:\\x\\Login Data"));
        assert!(is_password_store("C:\\x\\logins.json"));
        assert!(!is_password_store("C:\\x\\data.bin"));
    }

    #[test]
    fn temp_file_can_be_deleted_but_only_inside_roots() {
        let dir = std::env::temp_dir().join("cdc-delete-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.bin");
        std::fs::write(&file, b"hello").unwrap();
        let path = file.to_string_lossy().to_string();
        let roots = vec![dir.to_string_lossy().to_string()];

        // 不在允许的根目录里：必须跳过。
        let other = vec![std::env::temp_dir().join("cdc-other").to_string_lossy().to_string()];
        assert!(matches!(delete_file_checked(&path, &other, true), DeleteOutcome::Skipped));
        assert!(Path::new(&path).exists());

        // 在根目录里、位于 Temp 下：允许删除。
        assert!(matches!(delete_file_checked(&path, &roots, true), DeleteOutcome::Deleted));
        assert!(!Path::new(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_library_and_system_detection() {
        assert!(is_system_path("C:\\Windows\\Temp"));
        assert!(!is_system_path("C:\\Users\\Administrator\\AppData\\Local\\Temp"));
        assert!(is_within(
            "C:\\Users\\Administrator\\AppData\\Local\\Temp\\a\\b.txt",
            &["C:\\Users\\Administrator\\AppData\\Local\\Temp".to_string()]
        ));
    }
}
