//! 碎片清理：调用系统 defrag.exe 做分析和优化，并解析它的中英文输出。
//! defrag 的输出是控制台编码（OEM 代码页），需要先转成 UTF-16 再解析。

use serde::Serialize;
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
const OPEN_EXISTING: u32 = 3;
const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x1 | 0x2 | 0x4;
const INVALID_HANDLE_VALUE: isize = -1;

#[derive(Serialize)]
pub struct FragReport {
    pub drive: String,
    pub percent: Option<f64>,
    pub space_gb: Option<f64>,
    pub count: Option<u64>,
    pub free_gb: Option<f64>,
    pub ssd: Option<bool>,
}

#[repr(C)]
struct StoragePropertyQuery {
    property_id: u32,
    query_type: u32,
    additional: [u8; 1],
}

#[repr(C)]
struct SeekPenaltyDescriptor {
    version: u32,
    size: u32,
    incurs_seek_penalty: u8,
}

#[link(name = "kernel32")]
extern "system" {
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        text: *const u8,
        text_len: i32,
        out: *mut u16,
        out_len: i32,
    ) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        creation: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn DeviceIoControl(
        device: *mut c_void,
        code: u32,
        in_buffer: *mut c_void,
        in_size: u32,
        out_buffer: *mut c_void,
        out_size: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

pub fn defrag_path() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{}\\System32\\defrag.exe", root.trim_end_matches('\\'))
}

pub fn analyze(drive: &str) -> FragReport {
    let ssd = is_ssd(drive);
    let output = Command::new(defrag_path())
        .arg(drive)
        .arg("/A")
        .arg("/V")
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let text = match output {
        Ok(output) => {
            let stdout = decode_console(&output.stdout);
            if stdout.trim().is_empty() {
                decode_console(&output.stderr)
            } else {
                stdout
            }
        }
        Err(_) => String::new(),
    };
    let parsed = parse_defrag(&text);
    FragReport {
        drive: drive.to_string(),
        percent: parsed.percent,
        space_gb: parsed.space_gb,
        count: parsed.count,
        free_gb: parsed.free_gb,
        ssd,
    }
}

pub fn optimize(drive: &str) -> Result<(), String> {
    let status = Command::new(defrag_path())
        .arg(drive)
        .arg("/O")
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => Err("磁盘优化失败，请检查权限或系统工具状态".to_string()),
    }
}

struct ParsedFrag {
    percent: Option<f64>,
    space_gb: Option<f64>,
    count: Option<u64>,
    free_gb: Option<f64>,
}

fn parse_defrag(text: &str) -> ParsedFrag {
    let folded = text.replace('\u{3000}', " ");
    ParsedFrag {
        percent: search_percent(&folded),
        space_gb: number_with_unit(
            &folded,
            &["碎片空间", "文件碎片", "Total fragmented space", "碎片文件"],
            "GB",
        ),
        count: search_count(&folded),
        free_gb: number_with_unit(&folded, &["可用空间", "Free space"], "GB"),
    }
}

fn search_percent(text: &str) -> Option<f64> {
    let labels = [
        "碎片率",
        "碎片总计",
        "文件碎片百分比",
        "Percent file fragmentation",
    ];
    if let Some(value) = number_with_unit(text, &labels, "%") {
        return Some(value);
    }
    if let Some(value) = number_with_unit(text, &["fragmented", "碎片"], "%") {
        return Some(value);
    }
    // 旧版输出：第一个「xxx = 12%」通常就是碎片率，跳过可用空间那一行。
    for line in text.lines() {
        if line.contains('=') && line.contains('%') {
            if line.contains("可用") || line.to_lowercase().contains("free") {
                continue;
            }
            if let Some((value, _end)) = next_number(line) {
                return Some(value);
            }
        }
    }
    None
}

fn search_count(text: &str) -> Option<u64> {
    let labels = [
        "碎片文件数",
        "总碎片数",
        "碎片数",
        "Fragmented files",
        "Total fragments",
        // 中文版 defrag 的「文件:」段里写的是「碎片文件 = 27290」。
        "碎片文件",
    ];
    let after = find_after(text, &labels)?;
    let window = window_chars(after, 60);
    let (value, _end) = next_number(window)?;
    Some(value as u64)
}

fn number_with_unit(text: &str, labels: &[&str], unit: &str) -> Option<f64> {
    let after = find_after(text, labels)?;
    let window = window_chars(after, 60);
    let (value, end) = next_number(window)?;
    let rest = window[end..].trim_start();
    let rest_upper = rest.to_uppercase();
    if rest_upper.starts_with(&unit.to_uppercase()) {
        Some(value)
    } else {
        None
    }
}

fn window_chars(text: &str, count: usize) -> &str {
    match text.char_indices().nth(count) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

fn find_after<'a>(text: &'a str, labels: &[&str]) -> Option<&'a str> {
    let lower = text.to_lowercase();
    for label in labels {
        let needle = label.to_lowercase();
        if let Some(position) = lower.find(&needle) {
            let start = position + needle.len();
            if start <= text.len() {
                return Some(&text[start..]);
            }
        }
    }
    None
}

fn next_number(text: &str) -> Option<(f64, usize)> {
    let mut index = 0;
    while index < text.len() {
        let ch = text[index..].chars().next()?;
        if ch.is_ascii_digit() {
            let start = index;
            let mut end = index + ch.len_utf8();
            while end < text.len() {
                let next = text[end..].chars().next()?;
                if next.is_ascii_digit() || next == '.' || next == ',' {
                    end += next.len_utf8();
                } else {
                    break;
                }
            }
            let raw: String = text[start..end].chars().filter(|c| *c != ',').collect();
            if let Ok(value) = raw.parse::<f64>() {
                return Some((value, end));
            }
            index = end;
            continue;
        }
        index += ch.len_utf8();
    }
    None
}

fn decode_console(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    const CP_OEMCP: u32 = 1;
    unsafe {
        let length = MultiByteToWideChar(
            CP_OEMCP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if length <= 0 {
            return String::from_utf8_lossy(bytes).to_string();
        }
        let mut buffer = vec![0u16; length as usize];
        let written = MultiByteToWideChar(
            CP_OEMCP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            buffer.as_mut_ptr(),
            length,
        );
        String::from_utf16_lossy(&buffer[..written.max(0) as usize])
    }
}

/// 查询磁盘是否固态盘（没有寻道开销）。判断不了返回 None。
pub fn is_ssd(drive: &str) -> Option<bool> {
    let letter = drive.trim_end_matches(['\\', '/']).chars().next()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    let name: Vec<u16> = format!("\\\\.\\{}:", letter)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            0,
            FILE_SHARE_READ_WRITE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut query = StoragePropertyQuery {
        property_id: STORAGE_DEVICE_SEEK_PENALTY_PROPERTY,
        query_type: 0,
        additional: [0],
    };
    let mut descriptor = SeekPenaltyDescriptor {
        version: 0,
        size: 0,
        incurs_seek_penalty: 0,
    };
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &mut query as *mut _ as *mut c_void,
            std::mem::size_of::<StoragePropertyQuery>() as u32,
            &mut descriptor as *mut _ as *mut c_void,
            std::mem::size_of::<SeekPenaltyDescriptor>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    Some(descriptor.incurs_seek_penalty == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chinese_report() {
        let text = "卷信息:\n  卷大小 = 118.99 GB\n  可用空间 = 15.23 GB\n  碎片率 = 12%\n  碎片空间 = 0.42 GB\n  碎片文件数 = 1,234\n";
        let parsed = parse_defrag(text);
        assert_eq!(parsed.percent, Some(12.0));
        assert_eq!(parsed.space_gb, Some(0.42));
        assert_eq!(parsed.count, Some(1234));
        assert_eq!(parsed.free_gb, Some(15.23));
    }

    #[test]
    fn parses_english_report() {
        let text = "Percent file fragmentation = 7%\nTotal fragmented space = 0.10 GB\nTotal fragmented files = 42\nFree space = 30.5 GB\n";
        let parsed = parse_defrag(text);
        assert_eq!(parsed.percent, Some(7.0));
        assert_eq!(parsed.space_gb, Some(0.10));
        assert_eq!(parsed.count, Some(42));
        assert_eq!(parsed.free_gb, Some(30.5));
    }

    #[test]
    fn missing_fields_stay_none() {
        let parsed = parse_defrag("没有可用的分析数据");
        assert!(parsed.percent.is_none());
        assert!(parsed.count.is_none());
    }
}
