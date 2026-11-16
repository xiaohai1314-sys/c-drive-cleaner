//! 进程检测与管理：检测目标软件（微信、QQ、浏览器等）是否在运行，
//! 避免清理时因文件占用导致失败或破坏数据库/配置文件。

use std::collections::HashSet;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

/// 获取当前所有正在运行的进程名称集合（统一转为小写，如 "wechat.exe", "chrome.exe"）。
pub fn running_processes() -> HashSet<String> {
    let mut names = HashSet::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return names;
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let null_pos = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let exe_name = String::from_utf16_lossy(&entry.szExeFile[..null_pos])
                    .trim()
                    .to_lowercase();
                if !exe_name.is_empty() {
                    names.insert(exe_name);
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    names
}

/// 检查指定的进程列表中哪些正在运行。
pub fn check_running(targets: &[&str]) -> Vec<String> {
    let running = running_processes();
    targets
        .iter()
        .map(|t| t.to_lowercase())
        .filter(|t| running.contains(t.as_str()))
        .collect()
}

/// 关闭指定的进程（按可执行文件名，如 "wechat.exe"）。
pub fn kill_process_by_name(target_name: &str) -> usize {
    let target = target_name.trim().to_lowercase();
    let mut killed = 0;
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return 0;
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let null_pos = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let exe_name = String::from_utf16_lossy(&entry.szExeFile[..null_pos])
                    .trim()
                    .to_lowercase();
                if exe_name == target {
                    let handle = OpenProcess(PROCESS_TERMINATE, 0, entry.th32ProcessID);
                    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                        if TerminateProcess(handle, 1) != 0 {
                            killed += 1;
                        }
                        CloseHandle(handle);
                    }
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    killed
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_running_processes() {
        let procs = running_processes();
        assert!(!procs.is_empty());
    }
}
