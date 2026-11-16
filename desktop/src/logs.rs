//! 清理日志：每次清理写一份 `YYYYMMDD_HHMMSS.log`，历史页读取。

use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::timeutil;

pub fn log_dir() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let candidate = if base.ends_with("debug") || base.ends_with("release") {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../logs")
    } else {
        base.join("logs")
    };
    let _ = std::fs::create_dir_all(&candidate);
    candidate
}

pub fn write_log(lines: &[String]) -> Option<PathBuf> {
    let dir = log_dir();
    let stamp = timeutil::stamp();
    let mut path = dir.join(format!("{}.log", stamp));
    let mut index = 2;
    while path.exists() {
        path = dir.join(format!("{}_{}.log", stamp, index));
        index += 1;
    }
    let mut file = std::fs::File::create(&path).ok()?;
    for line in lines {
        let _ = writeln!(file, "{}", line.trim_end());
    }
    Some(path)
}

pub fn append_line(name: &str, line: &str) {
    let dir = log_dir();
    let path = dir.join(name);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", line);
    }
}

#[derive(Serialize)]
pub struct LogEntry {
    pub name: String,
    pub size: u64,
    pub mtime: u64,
}

pub fn list() -> Vec<LogEntry> {
    let dir = log_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut rows: Vec<LogEntry> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            Some(LogEntry {
                name,
                size: meta.len(),
                mtime,
            })
        })
        .collect();
    rows.sort_by(|left, right| right.mtime.cmp(&left.mtime));
    rows
}

/// 只允许读取日志目录里的普通文件，防止路径穿越。
pub fn read(name: &str) -> Result<String, String> {
    if name.trim().is_empty() || name.contains(['\\', '/']) || name.contains("..") {
        return Err("日志名不合法".to_string());
    }
    let path: PathBuf = log_dir().join(name);
    if !Path::new(&path).is_file() {
        return Err("日志不存在".to_string());
    }
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        assert!(read("../rules.json").is_err());
        assert!(read("sub\\file.log").is_err());
        assert!(read("..\\..\\windows\\win.ini").is_err());
    }

    #[test]
    fn writes_and_lists_a_log() {
        let path = write_log(&["第一行".to_string(), "第二行".to_string()]).expect("写日志失败");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let text = read(&name).expect("读日志失败");
        assert!(text.contains("第一行"));
        assert!(list().iter().any(|entry| entry.name == name));
        let _ = std::fs::remove_file(path);
    }
}
