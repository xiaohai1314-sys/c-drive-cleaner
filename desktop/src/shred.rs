//! 文件粉碎：随机数据覆盖写入后删除，不走回收站。
//! 系统目录、受保护路径和重解析点一律拒绝；目录逐个文件粉碎后清空。

use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use crate::safety;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[derive(Serialize)]
pub struct ShredReport {
    pub freed: u64,
    pub files_ok: u64,
    pub files_failed: u64,
    pub finished: u64,
    pub errors: Vec<String>,
}

pub fn shred_paths(paths: Vec<String>, passes: u32) -> ShredReport {
    let mut report = ShredReport {
        freed: 0,
        files_ok: 0,
        files_failed: 0,
        finished: 0,
        errors: Vec::new(),
    };
    for path in paths {
        if path.trim().is_empty() || !Path::new(&path).exists() {
            report.files_failed += 1;
            report.errors.push(format!("文件不存在：{}", path));
            continue;
        }
        if safety::is_reparse_point(&path) {
            report.files_failed += 1;
            report.errors.push(format!("不能粉碎符号链接或目录联接：{}", path));
            continue;
        }
        if safety::is_protected_path(&path) {
            report.files_failed += 1;
            report.errors.push(format!("为保护系统，拒绝粉碎：{}", path));
            continue;
        }
        if Path::new(&path).is_dir() {
            shred_directory(&path, passes, &mut report);
        } else {
            match shred_file(&path, passes) {
                Ok(size) => {
                    report.freed += size;
                    report.files_ok += 1;
                }
                Err(message) => {
                    report.files_failed += 1;
                    report.errors.push(message);
                }
            }
        }
    }
    report
}

fn shred_directory(root: &str, passes: u32, report: &mut ShredReport) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|item| item.ok())
    {
        let path = entry.path().to_string_lossy().to_string();
        if entry.file_type().is_symlink() {
            continue;
        }
        if entry.file_type().is_file() {
            files.push(path);
        } else if entry.file_type().is_dir() {
            dirs.push(path);
        }
    }
    for path in files {
        if safety::is_protected_path(&path) {
            report.files_failed += 1;
            report.errors.push(format!("为保护系统，拒绝粉碎：{}", path));
            continue;
        }
        match shred_file(&path, passes) {
            Ok(size) => {
                report.freed += size;
                report.files_ok += 1;
            }
            Err(message) => {
                report.files_failed += 1;
                report.errors.push(message);
            }
        }
    }
    dirs.sort_by_key(|path| std::cmp::Reverse(path.len()));
    for dir in dirs {
        let _ = std::fs::remove_dir(dir);
    }
    if report.files_failed == 0 && std::fs::remove_dir(root).is_ok() {
        report.finished += 1;
    }
}

fn shred_file(path: &str, passes: u32) -> Result<u64, String> {
    // 覆盖写需要读写权限，句柄删除还需要 DELETE 权限。
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const DELETE_ACCESS: u32 = 0x0001_0000;
    let mut file = OpenOptions::new()
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE_ACCESS)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("无法安全打开文件：{}（{}）", path, error))?;
    let meta = file.metadata().map_err(|error| error.to_string())?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || meta.is_dir() {
        return Err(format!("不能粉碎符号链接或目录联接：{}", path));
    }
    let size = meta.len();
    let mut rng = Rng::new();
    let mut buffer = vec![0u8; 1 << 20];
    for _ in 0..passes.max(1) {
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("{}：{}", path, error))?;
        let mut left = size;
        while left > 0 {
            let block = left.min(buffer.len() as u64) as usize;
            rng.fill(&mut buffer[..block]);
            file.write_all(&buffer[..block])
                .map_err(|error| format!("覆盖写入失败：{}（{}）", path, error))?;
            left -= block as u64;
        }
        file.sync_all()
            .map_err(|error| format!("{}：{}", path, error))?;
    }
    if !safety::mark_delete(&file) {
        return Err(format!("覆盖后无法删除：{}", path));
    }
    Ok(size)
}

struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn fill(&mut self, buffer: &mut [u8]) {
        for chunk in buffer.chunks_mut(8) {
            let bytes = self.next().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shreds_a_temp_file() {
        let dir = std::env::temp_dir().join("cdc-shred-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("secret.bin");
        std::fs::write(&file, vec![7u8; 4096]).unwrap();
        let report = shred_paths(vec![file.to_string_lossy().to_string()], 1);
        assert_eq!(report.files_ok, 1);
        assert_eq!(report.files_failed, 0);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_protected_paths() {
        let report = shred_paths(vec!["C:\\Windows\\System32\\kernel32.dll".to_string()], 1);
        assert_eq!(report.files_ok, 0);
        assert!(report.files_failed >= 1);
    }
}
