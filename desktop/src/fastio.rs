//! 高速 Windows 文件读取原语：
//! 1. 卷句柄合并直读（Coalesced Raw Volume Read）：Stage 1 按物理 LCN 升序合并邻近 4KB 文件头，
//!    一次 ReadFile 捞出多份文件头并绕过文件系统路径解析与杀软逐文件 Hook；
//! 2. OpenFileById：按 MFT 枚举得到的 64 位 FRN 直接打开文件，零目录遍历开销；
//! 3. IOCP 异步重叠 I/O（FILE_FLAG_OVERLAPPED）：单线程向 NVMe/HDD 队列同时投递多路异步读。

use crate::hashcache::Digest;
use crate::mft::SKIP_FILE_ATTRS;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_RANDOM_ACCESS: u32 = 0x1000_0000;
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const ERROR_IO_PENDING: u32 = 997;
const INFINITE: u32 = 0xFFFF_FFFF;
const INVALID_HANDLE_PTR: *mut c_void = (-1isize) as *mut c_void;

#[repr(C)]
struct FileIdDescriptor {
    dw_size: u32,
    id_type: u32,
    file_id: u64,
    _pad: u64,
}

#[repr(C)]
#[derive(Default)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
struct ByHandleFileInformation {
    dw_file_attributes: u32,
    ft_creation_time: [u32; 2],
    ft_last_access_time: [u32; 2],
    ft_last_write_time: [u32; 2],
    dw_volume_serial_number: u32,
    n_file_size_high: u32,
    n_file_size_low: u32,
    n_number_of_links: u32,
    n_file_index_high: u32,
    n_file_index_low: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        creation: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn OpenFileById(
        volume_hint: *mut c_void,
        file_id: *const FileIdDescriptor,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut c_void,
        flags_and_attributes: u32,
    ) -> *mut c_void;
    fn CreateIoCompletionPort(
        file_handle: *mut c_void,
        existing_completion_port: *mut c_void,
        completion_key: usize,
        number_of_concurrent_threads: u32,
    ) -> *mut c_void;
    fn GetQueuedCompletionStatus(
        completion_port: *mut c_void,
        bytes_transferred: *mut u32,
        completion_key: *mut usize,
        overlapped: *mut *mut Overlapped,
        milliseconds: u32,
    ) -> i32;
    fn ReadFile(
        file: *mut c_void,
        buffer: *mut c_void,
        to_read: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CancelIoEx(file: *mut c_void, overlapped: *mut Overlapped) -> i32;
    fn GetFileInformationByHandle(
        handle: *mut c_void,
        info: *mut ByHandleFileInformation,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

struct WinHandle(*mut c_void);

impl Drop for WinHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_PTR {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn open_drive_hint(drive_letter: char) -> Option<File> {
    let root = format!("{}:\\", drive_letter.to_ascii_uppercase());
    let w = wide(&root);
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if h.is_null() || h == INVALID_HANDLE_PTR {
        None
    } else {
        Some(unsafe { File::from_raw_handle(h) })
    }
}

fn open_raw_handle_by_id_or_path(
    hint: Option<&File>,
    frn: u64,
    path: &Path,
    flags: u32,
) -> Option<*mut c_void> {
    if frn != 0 {
        if let Some(hint_file) = hint {
            let desc = FileIdDescriptor {
                dw_size: std::mem::size_of::<FileIdDescriptor>() as u32,
                id_type: 0,
                file_id: frn,
                _pad: 0,
            };
            let h = unsafe {
                OpenFileById(
                    hint_file.as_raw_handle(),
                    &desc,
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null_mut(),
                    flags,
                )
            };
            if !h.is_null() && h != INVALID_HANDLE_PTR {
                return Some(h);
            }
        }
    }
    let w = wide(&path.to_string_lossy());
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if h.is_null() || h == INVALID_HANDLE_PTR {
        None
    } else {
        Some(h)
    }
}

pub fn open_checked_file_fast(
    hint: Option<&File>,
    frn: u64,
    path: &Path,
    flags: u32,
) -> Option<(File, Option<(u32, u64)>, u64)> {
    let file = if frn != 0 {
        if let Some(h) = open_raw_handle_by_id_or_path(hint, frn, path, flags) {
            unsafe { File::from_raw_handle(h) }
        } else {
            OpenOptions::new()
                .read(true)
                .custom_flags(flags)
                .open(path)
                .ok()?
        }
    } else {
        OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(path)
            .ok()?
    };

    let mut info = ByHandleFileInformation::default();
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok != 0 {
        if info.dw_file_attributes & SKIP_FILE_ATTRS != 0 {
            return None;
        }
        let file_index = ((info.n_file_index_high as u64) << 32) | (info.n_file_index_low as u64);
        let file_id = if info.n_number_of_links > 1 && file_index != 0 {
            Some((info.dw_volume_serial_number, file_index))
        } else {
            None
        };
        let actual_size = ((info.n_file_size_high as u64) << 32) | (info.n_file_size_low as u64);
        return Some((file, file_id, actual_size));
    }
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some((file, None, size))
}

/// 仅查询文件元数据标识（不读取文件内容、不触发安全软件内容扫描），
/// 用于在热缓存路径中兜底识别硬链接。
pub fn query_file_identity(path: &Path) -> Option<(u32, u64)> {
    let w = wide(&path.to_string_lossy());
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if h.is_null() || h == INVALID_HANDLE_PTR {
        return None;
    }
    let handle = WinHandle(h);
    let mut info = ByHandleFileInformation::default();
    let ok = unsafe { GetFileInformationByHandle(handle.0, &mut info) };
    if ok == 0 {
        return None;
    }
    let file_index = ((info.n_file_index_high as u64) << 32) | (info.n_file_index_low as u64);
    if info.n_number_of_links > 1 && file_index != 0 {
        Some((info.dw_volume_serial_number, file_index))
    } else {
        None
    }
}

pub struct RawHeadTarget {
    pub idx: usize,
    pub offset: u64,
    pub want: usize,
}

/// 按物理 LCN 合并读取同一卷上邻近文件的 4KB 头部（直接读卷句柄 `\\.\X:`）。
/// 返回已成功算出 4KB 摘要的 `(item_idx, digest)`；失败或未覆写到的项自动由上层回退到 IOCP/OpenFileById。
pub fn coalesced_raw_volume_heads(
    drive_letter: char,
    mut targets: Vec<RawHeadTarget>,
    is_ssd: bool,
    cancel: &AtomicBool,
    on_tick: &dyn Fn(),
) -> Vec<(usize, Digest)> {
    if targets.is_empty() {
        return Vec::new();
    }
    let vol_path = format!("\\\\.\\{}:", drive_letter.to_ascii_uppercase());
    let Ok(mut vol) = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(if is_ssd {
            FILE_FLAG_RANDOM_ACCESS
        } else {
            FILE_FLAG_SEQUENTIAL_SCAN
        })
        .open(&vol_path)
    else {
        return Vec::new();
    };

    targets.sort_by_key(|t| t.offset);

    let max_gap: u64 = if is_ssd { 16 * 1024 } else { 256 * 1024 };
    let max_span: u64 = if is_ssd { 256 * 1024 } else { 512 * 1024 };

    let mut out = Vec::with_capacity(targets.len());
    let mut buf = vec![0u8; max_span as usize];
    let mut i = 0usize;

    while i < targets.len() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let start_off = targets[i].offset;
        let mut end_off = start_off + 4096;
        let mut j = i + 1;
        while j < targets.len() {
            let next_off = targets[j].offset;
            let next_end = next_off + 4096;
            if next_off <= end_off + max_gap && next_end - start_off <= max_span {
                if next_end > end_off {
                    end_off = next_end;
                }
                j += 1;
            } else {
                break;
            }
        }

        let span_len = (end_off - start_off) as usize;
        if vol.seek(SeekFrom::Start(start_off)).is_ok() && vol.read_exact(&mut buf[..span_len]).is_ok()
        {
            for item in &targets[i..j] {
                let rel = (item.offset - start_off) as usize;
                let end = rel + item.want;
                if end <= span_len {
                    let digest = *blake3::hash(&buf[rel..end]).as_bytes();
                    out.push((item.idx, digest));
                    on_tick();
                }
            }
        }
        i = j;
    }

    out
}

pub struct IocpHeadRequest {
    pub idx: usize,
    pub path: String,
    pub frn: u64,
    pub want: usize,
}

struct IocpSlot {
    overlapped: Overlapped,
    handle: *mut c_void,
    item_idx: usize,
    want: usize,
    active: bool,
    buf: [u8; 4096],
}

/// 使用 OpenFileById + Windows IOCP 异步重叠 I/O 批量读取文件前 4KB。
/// 单线程内即可同时维持 `concurrency` 路在途 I/O（SSD=32，HDD=8），消除同步等待与线程切换开销。
pub fn iocp_read_heads(
    requests: Vec<IocpHeadRequest>,
    concurrency: usize,
    cancel: &AtomicBool,
    on_tick: &dyn Fn(),
) -> Vec<(usize, Digest, Option<(u32, u64)>)> {
    if requests.is_empty() {
        return Vec::new();
    }
    let port_raw = unsafe { CreateIoCompletionPort(INVALID_HANDLE_PTR, std::ptr::null_mut(), 0, 0) };
    if port_raw.is_null() {
        return Vec::new();
    }
    let port = WinHandle(port_raw);

    let mut hints: HashMap<char, File> = HashMap::new();
    for req in &requests {
        if let Some(ch) = req.path.chars().next() {
            let up = ch.to_ascii_uppercase();
            if !hints.contains_key(&up) && req.frn != 0 {
                if let Some(h) = open_drive_hint(up) {
                    hints.insert(up, h);
                }
            }
        }
    }

    let pool_size = concurrency.clamp(1, 64).min(requests.len());
    let mut slots: Vec<Box<IocpSlot>> = (0..pool_size)
        .map(|_| {
            Box::new(IocpSlot {
                overlapped: Overlapped::default(),
                handle: std::ptr::null_mut(),
                item_idx: 0,
                want: 0,
                active: false,
                buf: [0u8; 4096],
            })
        })
        .collect();
    let mut free_slots: Vec<usize> = (0..pool_size).rev().collect();
    let mut results = Vec::with_capacity(requests.len());
    let mut req_iter = requests.into_iter();
    let mut in_flight = 0usize;

    let submit_next = |slot_idx: usize,
                       slot: &mut IocpSlot,
                       iter: &mut std::vec::IntoIter<IocpHeadRequest>,
                       results: &mut Vec<(usize, Digest, Option<(u32, u64)>)>|
     -> bool {
        while let Some(req) = iter.next() {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let drive_ch = req.path.chars().next().unwrap_or('C').to_ascii_uppercase();
            let hint = hints.get(&drive_ch);
            let flags = FILE_FLAG_OVERLAPPED | FILE_FLAG_RANDOM_ACCESS;
            let Some(h) = open_raw_handle_by_id_or_path(hint, req.frn, Path::new(&req.path), flags)
            else {
                on_tick();
                continue;
            };

            let mut file_id = None;
            if req.frn == 0 {
                let mut info = ByHandleFileInformation::default();
                if unsafe { GetFileInformationByHandle(h, &mut info) } != 0 {
                    if info.dw_file_attributes & SKIP_FILE_ATTRS != 0 {
                        unsafe { CloseHandle(h) };
                        on_tick();
                        continue;
                    }
                    let fid = ((info.n_file_index_high as u64) << 32) | (info.n_file_index_low as u64);
                    if info.n_number_of_links > 1 && fid != 0 {
                        file_id = Some((info.dw_volume_serial_number, fid));
                    }
                }
            }
            let _ = file_id;

            let bound = unsafe { CreateIoCompletionPort(h, port.0, slot_idx, 0) };
            if bound.is_null() {
                unsafe { CloseHandle(h) };
                on_tick();
                continue;
            }

            let want = req.want.min(4096);
            if want == 0 {
                let digest = *blake3::hash(&[]).as_bytes();
                unsafe { CloseHandle(h) };
                results.push((req.idx, digest, None));
                on_tick();
                continue;
            }

            slot.overlapped = Overlapped::default();
            slot.handle = h;
            slot.item_idx = req.idx;
            slot.want = want;
            slot.active = true;

            let mut dummy_read: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    h,
                    slot.buf.as_mut_ptr() as *mut c_void,
                    want as u32,
                    &mut dummy_read,
                    &mut slot.overlapped as *mut Overlapped as *mut c_void,
                )
            };
            if ok == 0 {
                let err = unsafe { GetLastError() };
                if err != ERROR_IO_PENDING {
                    slot.active = false;
                    slot.handle = std::ptr::null_mut();
                    unsafe { CloseHandle(h) };
                    on_tick();
                    continue;
                }
            }
            return true;
        }
        false
    };

    while let Some(slot_idx) = free_slots.pop() {
        if submit_next(slot_idx, &mut slots[slot_idx], &mut req_iter, &mut results) {
            in_flight += 1;
        } else {
            free_slots.push(slot_idx);
            break;
        }
    }

    while in_flight > 0 {
        let mut bytes_transferred: u32 = 0;
        let mut completion_key: usize = 0;
        let mut ov_ptr: *mut Overlapped = std::ptr::null_mut();
        let ok = unsafe {
            GetQueuedCompletionStatus(
                port.0,
                &mut bytes_transferred,
                &mut completion_key,
                &mut ov_ptr,
                INFINITE,
            )
        };
        if ov_ptr.is_null() {
            break;
        }
        let slot_idx = completion_key;
        let slot = &mut slots[slot_idx];
        let mut file_id = None;
        if ok != 0 && bytes_transferred > 0 {
            let n = (bytes_transferred as usize).min(slot.want);
            let digest = *blake3::hash(&slot.buf[..n]).as_bytes();
            let mut info = ByHandleFileInformation::default();
            if unsafe { GetFileInformationByHandle(slot.handle, &mut info) } != 0 {
                let fid = ((info.n_file_index_high as u64) << 32) | (info.n_file_index_low as u64);
                if info.n_number_of_links > 1 && fid != 0 {
                    file_id = Some((info.dw_volume_serial_number, fid));
                }
            }
            results.push((slot.item_idx, digest, file_id));
        }
        if !slot.handle.is_null() {
            unsafe { CloseHandle(slot.handle) };
            slot.handle = std::ptr::null_mut();
        }
        slot.active = false;
        in_flight -= 1;
        on_tick();

        if !cancel.load(Ordering::Relaxed)
            && submit_next(slot_idx, slot, &mut req_iter, &mut results)
        {
            in_flight += 1;
        } else {
            // 若已取消，则不再继续投递，等剩余在途请求全部落袋后退出。
            for s in slots.iter_mut() {
                if s.active && cancel.load(Ordering::Relaxed) && !s.handle.is_null() {
                    unsafe { CancelIoEx(s.handle, std::ptr::null_mut()) };
                }
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn open_by_id_and_iocp_read_match_direct_file_bytes() {
        let dir = std::env::temp_dir().join("cdc-fastio-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let p1 = dir.join("a.bin");
        let p2 = dir.join("b.bin");
        let d1: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
        let d2: Vec<u8> = (0..8192).map(|i| ((i * 7) % 241) as u8).collect();
        std::fs::write(&p1, &d1).unwrap();
        std::fs::write(&p2, &d2).unwrap();

        // 从普通打开拿到真实的 FileIndex 作为 FRN 测试 OpenFileById。
        let (_, _, s1) = open_checked_file_fast(None, 0, &p1, FILE_FLAG_RANDOM_ACCESS).unwrap();
        assert_eq!(s1, 8192);
        let f1 = File::open(&p1).unwrap();
        let mut info = ByHandleFileInformation::default();
        assert_ne!(unsafe { GetFileInformationByHandle(f1.as_raw_handle(), &mut info) }, 0);
        let frn1 = ((info.n_file_index_high as u64) << 32) | (info.n_file_index_low as u64);
        drop(f1);

        let drive = p1.to_string_lossy().chars().next().unwrap();
        let hint = open_drive_hint(drive);
        let (mut opened_by_id, _, _) =
            open_checked_file_fast(hint.as_ref(), frn1, &p1, FILE_FLAG_RANDOM_ACCESS).unwrap();
        let mut head = [0u8; 4096];
        opened_by_id.read_exact(&mut head).unwrap();
        assert_eq!(&head[..], &d1[..4096]);

        let cancel = AtomicBool::new(false);
        let ticks = AtomicUsize::new(0);
        let res = iocp_read_heads(
            vec![
                IocpHeadRequest {
                    idx: 0,
                    path: p1.to_string_lossy().to_string(),
                    frn: frn1,
                    want: 4096,
                },
                IocpHeadRequest {
                    idx: 1,
                    path: p2.to_string_lossy().to_string(),
                    frn: 0,
                    want: 4096,
                },
            ],
            4,
            &cancel,
            &|| {
                ticks.fetch_add(1, Ordering::Relaxed);
            },
        );
        assert_eq!(res.len(), 2);
        assert_eq!(ticks.load(Ordering::Relaxed), 2);
        let h1 = res.iter().find(|r| r.0 == 0).unwrap().1;
        let h2 = res.iter().find(|r| r.0 == 1).unwrap().1;
        assert_eq!(h1, *blake3::hash(&d1[..4096]).as_bytes());
        assert_eq!(h2, *blake3::hash(&d2[..4096]).as_bytes());

        let _ = std::fs::remove_dir_all(&dir);
    }
}