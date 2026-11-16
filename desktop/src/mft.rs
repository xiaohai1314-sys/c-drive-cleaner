//! 高性能文件枚举：
//! 1. 优先直读 NTFS $MFT，按 8MB 块交由 Rayon 多核并行解析；
//!    在解码文件名之前先按文件属性与大小剪枝，消除 95%+ 小文件无效 UTF-16 分配；
//! 2. 导出 FRN、首簇 LCN 与集群字节数，供上层走卷句柄 LCN 合并直读与 OpenFileById；
//! 3. 配合二进制快照 + USN Journal 增量更新（FSCTL_READ_USN_JOURNAL +
//!    FSCTL_GET_NTFS_FILE_RECORD），非 NTFS 或无权限时退回并行/按盘顺序目录遍历。

use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const OPEN_EXISTING: u32 = 3;
const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x0009_0064;
const FSCTL_GET_NTFS_FILE_RECORD: u32 = 0x0009_0068;
const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;
const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
const INVALID_HANDLE_VALUE: isize = -1;
const INVALID_HANDLE_PTR: *mut c_void = INVALID_HANDLE_VALUE as *mut c_void;

const SNAPSHOT_MAGIC: &[u8; 4] = b"MFT3";
const SNAPSHOT_FLOOR_MIN_SIZE: u64 = 1 << 20;
const MAX_USN_DELTA_FRNS: usize = 25_000;
const RAW_READ_MIN_AGE_SECS: u64 = 300;

pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
pub const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0000_0800;
pub const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x0000_0200;
pub const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
pub const FILE_ATTRIBUTE_ENCRYPTED: u32 = 0x0000_4000;
pub const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
pub const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
pub const SKIP_FILE_ATTRS: u32 = FILE_ATTRIBUTE_REPARSE_POINT
    | FILE_ATTRIBUTE_OFFLINE
    | FILE_ATTRIBUTE_RECALL_ON_OPEN
    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
const NO_RAW_ATTRS: u32 =
    SKIP_FILE_ATTRS | FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_ENCRYPTED | FILE_ATTRIBUTE_SPARSE_FILE;

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub sort_key: u64,
    pub frn: u64,
    pub first_lcn: u64,
    pub bytes_per_cluster: u32,
    pub can_raw_read: bool,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct NtfsVolumeData {
    volume_serial_number: i64,
    number_sectors: i64,
    total_clusters: i64,
    free_clusters: i64,
    total_reserved: i64,
    bytes_per_sector: u32,
    bytes_per_cluster: u32,
    bytes_per_file_record_segment: u32,
    clusters_per_file_record_segment: u32,
    mft_valid_data_length: i64,
    mft_start_lcn: i64,
    mft2_start_lcn: i64,
    mft_zone_start: i64,
    mft_zone_end: i64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct UsnJournalData {
    usn_journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    lowest_valid_usn: i64,
    max_usn: i64,
    maximum_size: u64,
    allocation_delta: u64,
}

#[repr(C)]
struct ReadUsnJournalDataV0 {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    usn_journal_id: u64,
}

#[repr(C)]
struct NtfsFileRecordInput {
    file_reference_number: i64,
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
    fn SetFilePointerEx(
        file: *mut c_void,
        distance: i64,
        new_pointer: *mut i64,
        method: u32,
    ) -> i32;
    fn ReadFile(
        file: *mut c_void,
        buffer: *mut c_void,
        to_read: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

struct VolumeHandle(*mut c_void);

impl Drop for VolumeHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_PTR {
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// 按文件名与大小在拼完整路径之前提前剪枝的过滤器。
pub type KeepFn<'a> = &'a (dyn Fn(&str, u64) -> bool + Sync);

pub fn scan_files(root: &str, min_size: u64, skip_top: &[&str]) -> Vec<FileEntry> {
    scan_files_where(root, min_size, skip_top, &|_, _| true)
}

/// 与 `scan_files` 相同，但 `keep(文件名, 大小)` 返回 false 的条目在解析父目录、
/// 拼接路径之前就被丢弃。大文件分档阈值这类只看扩展名的过滤放在这里最省。
pub fn scan_files_where(
    root: &str,
    min_size: u64,
    skip_top: &[&str],
    keep: KeepFn,
) -> Vec<FileEntry> {
    if let Some(entries) = try_mft_scan(root, min_size, skip_top, keep) {
        return entries;
    }
    fallback_walk_files(root, min_size, skip_top)
        .into_iter()
        .filter(|e| {
            let name = Path::new(&e.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            keep(&name, e.size)
        })
        .collect()
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| {
        u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    })
}

fn read_uint_le(bytes: &[u8]) -> i64 {
    let mut v: u64 = 0;
    for (i, b) in bytes.iter().enumerate() {
        v |= (*b as u64) << (8 * i);
    }
    v as i64
}

fn read_int_le(bytes: &[u8]) -> i64 {
    let mut v: u64 = 0;
    for (i, b) in bytes.iter().enumerate() {
        v |= (*b as u64) << (8 * i);
    }
    let bits = bytes.len() * 8;
    if bits < 64 && (v & (1u64 << (bits - 1))) != 0 {
        v |= !0u64 << bits;
    }
    v as i64
}

pub(crate) fn filetime_to_unix(ft: u64) -> u64 {
    const WINDOWS_EPOCH_DIFF_SECS: u64 = 11_644_473_600;
    (ft / 10_000_000).saturating_sub(WINDOWS_EPOCH_DIFF_SECS)
}

fn apply_fixups(rec: &mut [u8], bytes_per_sector: usize) {
    let Some(usa_off) = u16_at(rec, 4).map(|v| v as usize) else {
        return;
    };
    let Some(usa_count) = u16_at(rec, 6).map(|v| v as usize) else {
        return;
    };
    if usa_off == 0 || usa_count < 2 || usa_off + usa_count * 2 > rec.len() {
        return;
    }
    for i in 1..usa_count {
        let pos = i * bytes_per_sector;
        let src = usa_off + i * 2;
        if pos < 2 || pos > rec.len() || src + 2 > rec.len() {
            break;
        }
        rec[pos - 2] = rec[src];
        rec[pos - 1] = rec[src + 1];
    }
}

fn parse_runlist(buf: &[u8]) -> Vec<(i64, i64, i64)> {
    let mut runs = Vec::new();
    let mut vcn: i64 = 0;
    let mut prev_lcn: i64 = 0;
    let mut pos = 0;
    while pos < buf.len() {
        let header = buf[pos];
        pos += 1;
        if header == 0 {
            break;
        }
        let len_size = (header & 0x0F) as usize;
        let off_size = (header >> 4) as usize;
        if len_size == 0 || len_size > 8 || off_size > 8 || pos + len_size + off_size > buf.len() {
            break;
        }
        let run_len = read_uint_le(&buf[pos..pos + len_size]);
        pos += len_size;
        let off = if off_size == 0 {
            0
        } else {
            read_int_le(&buf[pos..pos + off_size])
        };
        pos += off_size;
        if off_size > 0 {
            prev_lcn += off;
            runs.push((vcn, prev_lcn, run_len));
        } else {
            runs.push((vcn, -1, run_len));
        }
        vcn += run_len;
    }
    runs
}

/// 返回首个 $DATA extent 的 `(first_lcn, run_clusters)`；稀疏或无效返回 `(0, 0)`。
pub(crate) fn parse_first_extent(buf: &[u8]) -> (u64, u64) {
    let Some(&header) = buf.first() else {
        return (0, 0);
    };
    if header == 0 {
        return (0, 0);
    }
    let len_size = (header & 0x0F) as usize;
    let off_size = (header >> 4) as usize;
    if len_size == 0
        || len_size > 8
        || off_size == 0
        || off_size > 8
        || 1 + len_size + off_size > buf.len()
    {
        return (0, 0);
    }
    let run_len = read_uint_le(&buf[1..1 + len_size]);
    let lcn = read_int_le(&buf[1 + len_size..1 + len_size + off_size]);
    if lcn > 0 && run_len > 0 {
        (lcn as u64, run_len as u64)
    } else {
        (0, 0)
    }
}

fn utf16le(bytes: &[u8]) -> String {
    let mut v = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        v.push(u16::from_le_bytes([bytes[i], bytes[i + 1]]));
        i += 2;
    }
    String::from_utf16_lossy(&v)
}

#[derive(Clone)]
struct DirNode {
    seq: u16,
    parent_frn: u64,
    name: String,
    reparse: bool,
}

#[derive(Clone)]
struct RawFile {
    frn: u64,
    parent_frn: u64,
    name: String,
    size: u64,
    mtime: u64,
    first_lcn: u64,
    sort_key: u64,
    can_raw_read: bool,
}

enum ParsedRecord {
    Skip,
    Dir {
        index: u64,
        node: DirNode,
    },
    File {
        index: u64,
        raw: RawFile,
    },
    MftRoot {
        runs: Vec<(i64, i64, i64)>,
    },
}

fn parse_record_in_place(
    rec: &mut [u8],
    index: u64,
    bytes_per_sector: usize,
    bytes_per_cluster: usize,
    min_size: u64,
) -> ParsedRecord {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return ParsedRecord::Skip;
    }
    apply_fixups(rec, bytes_per_sector);

    let Some(flags) = u16_at(rec, 22) else {
        return ParsedRecord::Skip;
    };
    if flags & 1 == 0 {
        return ParsedRecord::Skip;
    }
    if index != 0 && u64_at(rec, 32).unwrap_or(0) != 0 {
        return ParsedRecord::Skip;
    }

    let is_dir = flags & 2 != 0;
    let Some(sequence) = u16_at(rec, 16) else {
        return ParsedRecord::Skip;
    };
    let frn = ((sequence as u64) << 48) | index;
    let Some(first_attr) = u16_at(rec, 20).map(|v| v as usize) else {
        return ParsedRecord::Skip;
    };

    // 先只记录名字字节切片位置，等确认 size >= min_size 后再解码 UTF-16，避免百万小文件无效堆分配。
    let mut name_span: Option<(usize, usize)> = None;
    let mut name_is_dos = false;
    let mut parent_frn: u64 = 0;
    let mut size: u64 = 0;
    let mut mtime: u64 = 0;
    let mut attrs: u32 = 0;
    let mut data_flags: u16 = 0;
    let mut first_lcn: u64 = 0;
    let mut first_run_clusters: u64 = 0;
    let mut mft_runs: Option<Vec<(i64, i64, i64)>> = None;

    let mut pos = first_attr;
    while pos + 8 <= rec.len() {
        let Some(attr_type) = u32_at(rec, pos) else {
            break;
        };
        if attr_type == 0xFFFF_FFFF {
            break;
        }
        let Some(attr_len) = u32_at(rec, pos + 4).map(|v| v as usize) else {
            break;
        };
        if attr_len < 16 || pos + attr_len > rec.len() {
            break;
        }
        let non_resident = rec[pos + 8];
        let name_len = rec[pos + 9];

        if attr_type == 0x10 && non_resident == 0 {
            if let Some(val_off) = u16_at(rec, pos + 20) {
                let v = pos + val_off as usize;
                if v + 36 <= pos + attr_len {
                    if let Some(ft) = u64_at(rec, v + 8) {
                        mtime = filetime_to_unix(ft);
                    }
                    if let Some(a) = u32_at(rec, v + 32) {
                        attrs = a;
                        if !is_dir && (attrs & SKIP_FILE_ATTRS) != 0 {
                            return ParsedRecord::Skip;
                        }
                    }
                }
            }
        } else if attr_type == 0x30 && non_resident == 0 {
            if let Some(val_off) = u16_at(rec, pos + 20) {
                let v = pos + val_off as usize;
                if v + 66 <= pos + attr_len {
                    if let Some(parent_ref) = u64_at(rec, v) {
                        let nlen = rec[v + 64] as usize;
                        let namespace = rec[v + 65];
                        let name_off = v + 66;
                        let byte_len = nlen * 2;
                        if name_off + byte_len <= pos + attr_len {
                            let is_dos = namespace == 2;
                            if name_span.is_none()
                                || (name_is_dos && !is_dos)
                                || namespace == 0
                                || namespace == 1
                            {
                                name_span = Some((name_off, byte_len));
                                parent_frn = parent_ref;
                                name_is_dos = is_dos;
                            }
                        }
                    }
                }
            }
        } else if attr_type == 0x80 && name_len == 0 {
            data_flags = u16_at(rec, pos + 12).unwrap_or(0);
            if non_resident != 0 {
                if pos + 56 <= rec.len() && u64_at(rec, pos + 16) == Some(0) {
                    if let Some(real_size) = u64_at(rec, pos + 48) {
                        size = real_size;
                    }
                    if let Some(run_off) = u16_at(rec, pos + 32) {
                        let start = pos + run_off as usize;
                        if start <= pos + attr_len {
                            if index == 0 {
                                mft_runs = Some(parse_runlist(&rec[start..pos + attr_len]));
                            } else {
                                let (lcn, clusters) = parse_first_extent(&rec[start..pos + attr_len]);
                                first_lcn = lcn;
                                first_run_clusters = clusters;
                            }
                        }
                    }
                }
            } else if let Some(vlen) = u32_at(rec, pos + 16) {
                size = vlen as u64;
            }
        }

        pos += attr_len;
    }

    if index == 0 {
        return match mft_runs {
            Some(runs) if !runs.is_empty() => ParsedRecord::MftRoot { runs },
            _ => ParsedRecord::Skip,
        };
    }

    let Some((name_off, byte_len)) = name_span else {
        return ParsedRecord::Skip;
    };

    if is_dir {
        let name = utf16le(&rec[name_off..name_off + byte_len]);
        let reparse = (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
        return ParsedRecord::Dir {
            index,
            node: DirNode {
                seq: sequence,
                parent_frn,
                name,
                reparse,
            },
        };
    }

    if size < min_size || (attrs & SKIP_FILE_ATTRS) != 0 {
        return ParsedRecord::Skip;
    }

    let name = utf16le(&rec[name_off..name_off + byte_len]);
    if name.starts_with('$') {
        return ParsedRecord::Skip;
    }

    let first_extent_bytes = first_run_clusters.saturating_mul(bytes_per_cluster as u64);
    let can_raw_read = (attrs & NO_RAW_ATTRS) == 0
        && (data_flags & (0x0001 | 0x4000 | 0x8000)) == 0
        && first_lcn > 0
        && first_extent_bytes >= size.min(4096);

    ParsedRecord::File {
        index,
        raw: RawFile {
            frn,
            parent_frn,
            name,
            size,
            mtime,
            first_lcn,
            sort_key: if first_lcn > 0 { first_lcn } else { index },
            can_raw_read,
        },
    }
}
fn read_exact_at(handle: *mut c_void, offset: i64, buf: &mut [u8]) -> usize {
    unsafe {
        if SetFilePointerEx(handle, offset, std::ptr::null_mut(), 0) == 0 {
            return 0;
        }
    }
    let mut total = 0usize;
    while total < buf.len() {
        let mut read: u32 = 0;
        let to_read = (buf.len() - total).min(8 * 1024 * 1024) as u32;
        let ok = unsafe {
            ReadFile(
                handle,
                buf[total..].as_mut_ptr() as *mut c_void,
                to_read,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            break;
        }
        total += read as usize;
    }
    total
}

fn query_usn_journal(handle: *mut c_void) -> Option<UsnJournalData> {
    let mut data = UsnJournalData::default();
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null_mut(),
            0,
            &mut data as *mut _ as *mut c_void,
            std::mem::size_of::<UsnJournalData>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        None
    } else {
        Some(data)
    }
}

fn collect_usn_changed_indices(
    handle: *mut c_void,
    journal: &UsnJournalData,
    start_usn: i64,
) -> Option<HashSet<u64>> {
    let mut input = ReadUsnJournalDataV0 {
        start_usn,
        reason_mask: 0xFFFF_FFFF,
        return_only_on_close: 0,
        timeout: 0,
        bytes_to_wait_for: 0,
        usn_journal_id: journal.usn_journal_id,
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut changed = HashSet::new();

    loop {
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_READ_USN_JOURNAL,
                &mut input as *mut _ as *mut c_void,
                std::mem::size_of::<ReadUsnJournalDataV0>() as u32,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || returned < 8 {
            return None;
        }
        let len = returned as usize;
        let next_cursor = u64_at(&buf, 0)? as i64;
        if len == 8 {
            break;
        }
        let mut off = 8usize;
        while off + 60 <= len {
            let Some(rec_len) = u32_at(&buf, off).map(|v| v as usize) else {
                break;
            };
            if rec_len < 60 || off + rec_len > len {
                break;
            }
            let major = u16_at(&buf, off + 4).unwrap_or(0);
            if major == 2 || major == 3 {
                if let Some(frn) = u64_at(&buf, off + 8) {
                    changed.insert(frn & 0xFFFF_FFFF_FFFF);
                    if changed.len() > MAX_USN_DELTA_FRNS {
                        return None;
                    }
                }
            }
            off += rec_len;
        }
        if next_cursor <= input.start_usn || next_cursor >= journal.next_usn {
            break;
        }
        input.start_usn = next_cursor;
    }

    Some(changed)
}

fn read_single_mft_record(
    handle: *mut c_void,
    index: u64,
    record_size: usize,
    out_buf: &mut [u8],
) -> Option<(u64, usize)> {
    let mut input = NtfsFileRecordInput {
        file_reference_number: index as i64,
    };
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_NTFS_FILE_RECORD,
            &mut input as *mut _ as *mut c_void,
            std::mem::size_of::<NtfsFileRecordInput>() as u32,
            out_buf.as_mut_ptr() as *mut c_void,
            out_buf.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || (returned as usize) < 12 + record_size {
        return None;
    }
    let actual_frn = u64_at(out_buf, 0)?;
    let rec_len = u32_at(out_buf, 8)? as usize;
    if rec_len < record_size {
        return None;
    }
    Some((actual_frn, rec_len))
}

#[derive(Clone)]
struct MftSnapshot {
    volume_serial: u64,
    journal_id: u64,
    next_usn: i64,
    min_size: u64,
    bytes_per_cluster: u32,
    dirs: HashMap<u64, DirNode>,
    files: HashMap<u64, RawFile>,
}

/// 进程内快照缓存：大文件页与重复文件页共用，避免每次都从磁盘反序列化整份快照。
/// 以卷序列号为键；是否仍有效由调用方用 journal_id / next_usn 判断。
fn mem_snapshots() -> &'static Mutex<HashMap<u64, Arc<MftSnapshot>>> {
    static CELL: OnceLock<Mutex<HashMap<u64, Arc<MftSnapshot>>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mem_snapshot_get(serial: u64) -> Option<Arc<MftSnapshot>> {
    mem_snapshots().lock().ok()?.get(&serial).cloned()
}

fn mem_snapshot_put(serial: u64, snap: Arc<MftSnapshot>) {
    if let Ok(mut map) = mem_snapshots().lock() {
        map.insert(serial, snap);
    }
}

fn mem_snapshot_take(serial: u64) -> Option<Arc<MftSnapshot>> {
    mem_snapshots().lock().ok()?.remove(&serial)
}

/// 后台落盘快照，不阻塞本次扫描返回。同一时刻只允许一个写入，避免临时文件互相覆盖。
fn save_snapshot_in_background(path: PathBuf, snap: Arc<MftSnapshot>) {
    static SAVE_LOCK: Mutex<()> = Mutex::new(());
    std::thread::spawn(move || {
        let _guard = SAVE_LOCK.lock();
        let _ = save_snapshot(&path, &snap);
    });
}

fn snapshot_path(serial: u64) -> PathBuf {
    crate::cache_dir().join(format!("mft-v3-{:016x}.bin", serial))
}

fn write_u32(w: &mut impl Write, v: u32) -> std::io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn write_u64(w: &mut impl Write, v: u64) -> std::io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn write_str(w: &mut impl Write, s: &str) -> std::io::Result<()> {
    let bytes = s.as_bytes();
    write_u32(w, bytes.len() as u32)?;
    w.write_all(bytes)
}

fn read_u16(r: &mut impl Read) -> Option<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b).ok()?;
    Some(u16::from_le_bytes(b))
}
fn read_u32(r: &mut impl Read) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b))
}
fn read_u64(r: &mut impl Read) -> Option<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).ok()?;
    Some(u64::from_le_bytes(b))
}
fn read_str(r: &mut impl Read) -> Option<String> {
    let len = read_u32(r)? as usize;
    if len > 32 * 1024 {
        return None;
    }
    let mut b = vec![0u8; len];
    r.read_exact(&mut b).ok()?;
    String::from_utf8(b).ok()
}

fn save_snapshot(path: &Path, snap: &MftSnapshot) -> std::io::Result<()> {
    let tmp = path.with_extension("bin.tmp");
    {
        let file = std::fs::File::create(&tmp)?;
        let mut w = BufWriter::with_capacity(256 * 1024, file);
        w.write_all(SNAPSHOT_MAGIC)?;
        write_u64(&mut w, snap.volume_serial)?;
        write_u64(&mut w, snap.journal_id)?;
        write_u64(&mut w, snap.next_usn as u64)?;
        write_u64(&mut w, snap.min_size)?;
        write_u32(&mut w, snap.bytes_per_cluster)?;
        write_u32(&mut w, snap.dirs.len() as u32)?;
        for (&idx, node) in &snap.dirs {
            write_u64(&mut w, idx)?;
            w.write_all(&node.seq.to_le_bytes())?;
            w.write_all(&[u8::from(node.reparse)])?;
            write_u64(&mut w, node.parent_frn)?;
            write_str(&mut w, &node.name)?;
        }
        write_u32(&mut w, snap.files.len() as u32)?;
        for (&idx, f) in &snap.files {
            write_u64(&mut w, idx)?;
            write_u64(&mut w, f.frn)?;
            write_u64(&mut w, f.parent_frn)?;
            write_u64(&mut w, f.size)?;
            write_u64(&mut w, f.mtime)?;
            write_u64(&mut w, f.first_lcn)?;
            write_u64(&mut w, f.sort_key)?;
            w.write_all(&[u8::from(f.can_raw_read)])?;
            write_str(&mut w, &f.name)?;
        }
        w.flush()?;
    }
    let _ = std::fs::rename(&tmp, path);
    Ok(())
}

fn load_snapshot(path: &Path) -> Option<MftSnapshot> {
    let file = std::fs::File::open(path).ok()?;
    let mut r = BufReader::with_capacity(256 * 1024, file);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).ok()?;
    if &magic != SNAPSHOT_MAGIC {
        return None;
    }
    let volume_serial = read_u64(&mut r)?;
    let journal_id = read_u64(&mut r)?;
    let next_usn = read_u64(&mut r)? as i64;
    let min_size = read_u64(&mut r)?;
    let bytes_per_cluster = read_u32(&mut r)?;

    let dir_count = read_u32(&mut r)? as usize;
    let mut dirs = HashMap::with_capacity(dir_count);
    for _ in 0..dir_count {
        let idx = read_u64(&mut r)?;
        let seq = read_u16(&mut r)?;
        let mut flag = [0u8; 1];
        r.read_exact(&mut flag).ok()?;
        let parent_frn = read_u64(&mut r)?;
        let name = read_str(&mut r)?;
        dirs.insert(
            idx,
            DirNode {
                seq,
                parent_frn,
                name,
                reparse: flag[0] != 0,
            },
        );
    }

    let file_count = read_u32(&mut r)? as usize;
    let mut files = HashMap::with_capacity(file_count);
    for _ in 0..file_count {
        let idx = read_u64(&mut r)?;
        let frn = read_u64(&mut r)?;
        let parent_frn = read_u64(&mut r)?;
        let size = read_u64(&mut r)?;
        let mtime = read_u64(&mut r)?;
        let first_lcn = read_u64(&mut r)?;
        let sort_key = read_u64(&mut r)?;
        let mut raw_flag = [0u8; 1];
        r.read_exact(&mut raw_flag).ok()?;
        let name = read_str(&mut r)?;
        files.insert(
            idx,
            RawFile {
                frn,
                parent_frn,
                name,
                size,
                mtime,
                first_lcn,
                sort_key,
                can_raw_read: raw_flag[0] != 0,
            },
        );
    }

    Some(MftSnapshot {
        volume_serial,
        journal_id,
        next_usn,
        min_size,
        bytes_per_cluster,
        dirs,
        files,
    })
}

fn parse_chunk_parallel(
    data: &[u8],
    base_rec_idx: u64,
    record_size: usize,
    bytes_per_sector: usize,
    bytes_per_cluster: usize,
    min_size: u64,
    dirs: &mut HashMap<u64, DirNode>,
    files: &mut HashMap<u64, RawFile>,
) {
    let parsed: Vec<ParsedRecord> = data
        .par_chunks_exact(record_size)
        .enumerate()
        .map_init(
            || vec![0u8; record_size],
            |scratch, (slot, raw)| {
                if &raw[0..4] != b"FILE" {
                    return ParsedRecord::Skip;
                }
                scratch.copy_from_slice(raw);
                parse_record_in_place(
                    scratch,
                    base_rec_idx + slot as u64,
                    bytes_per_sector,
                    bytes_per_cluster,
                    min_size,
                )
            },
        )
        .collect();

    for rec in parsed {
        match rec {
            ParsedRecord::Dir { index, node } => {
                dirs.insert(index, node);
            }
            ParsedRecord::File { index, raw } => {
                files.insert(index, raw);
            }
            _ => {}
        }
    }
}
fn full_mft_read_parallel(
    handle: *mut c_void,
    volume: &NtfsVolumeData,
    runs: Vec<(i64, i64, i64)>,
    store_min_size: u64,
) -> Option<(HashMap<u64, DirNode>, HashMap<u64, RawFile>)> {
    let record_size = volume.bytes_per_file_record_segment as usize;
    let bytes_per_cluster = volume.bytes_per_cluster as usize;
    let bytes_per_sector = volume.bytes_per_sector as usize;
    let max_mft_bytes = if volume.mft_valid_data_length > 0 {
        volume.mft_valid_data_length as u64
    } else {
        u64::MAX
    };

    let mut dirs: HashMap<u64, DirNode> = HashMap::new();
    let mut files: HashMap<u64, RawFile> = HashMap::new();

    const CHUNK_SIZE: usize = 8 * 1024 * 1024;
    let mut read_buf = vec![0u8; CHUNK_SIZE];
    let mut parse_buf = vec![0u8; CHUNK_SIZE];
    let mut scratch = vec![0u8; record_size];
    let mut carry: Vec<u8> = Vec::with_capacity(record_size);
    let mut total_offset: u64 = 0;

    let mut pending_parse_len = 0usize;
    let mut pending_parse_base_idx = 0u64;

    for (_vcn, lcn, length) in runs {
        if total_offset >= max_mft_bytes {
            break;
        }
        if length <= 0 {
            continue;
        }
        let run_bytes = (length as u64).saturating_mul(bytes_per_cluster as u64);
        if lcn < 0 {
            if pending_parse_len > 0 {
                parse_chunk_parallel(
                    &parse_buf[..pending_parse_len],
                    pending_parse_base_idx,
                    record_size,
                    bytes_per_sector,
                    bytes_per_cluster,
                    store_min_size,
                    &mut dirs,
                    &mut files,
                );
                pending_parse_len = 0;
            }
            total_offset = total_offset.saturating_add(run_bytes);
            carry.clear();
            continue;
        }
        let disk_offset = lcn * bytes_per_cluster as i64;
        let to_process = run_bytes.min(max_mft_bytes - total_offset);
        let mut done_in_run: u64 = 0;

        while done_in_run < to_process {
            let want = ((to_process - done_in_run) as usize).min(CHUNK_SIZE);

            let got = if pending_parse_len > 0 {
                let parse_slice = &parse_buf[..pending_parse_len];
                let got_bytes = if rayon::current_thread_index().is_some() {
                    // 若调用方本身已在 Rayon 池内，使用 rayon::join 参与工作窃取，绝不阻塞占用线程池。
                    let handle_raw = handle as usize;
                    let (_, bytes) = rayon::join(
                        || {
                            parse_chunk_parallel(
                                parse_slice,
                                pending_parse_base_idx,
                                record_size,
                                bytes_per_sector,
                                bytes_per_cluster,
                                store_min_size,
                                &mut dirs,
                                &mut files,
                            );
                        },
                        || {
                            read_exact_at(
                                handle_raw as *mut c_void,
                                disk_offset + done_in_run as i64,
                                &mut read_buf[..want],
                            )
                        },
                    );
                    bytes
                } else {
                    let mut bytes = 0usize;
                    std::thread::scope(|s| {
                        let parser = s.spawn(|| {
                            parse_chunk_parallel(
                                parse_slice,
                                pending_parse_base_idx,
                                record_size,
                                bytes_per_sector,
                                bytes_per_cluster,
                                store_min_size,
                                &mut dirs,
                                &mut files,
                            );
                        });
                        bytes = read_exact_at(
                            handle,
                            disk_offset + done_in_run as i64,
                            &mut read_buf[..want],
                        );
                        let _ = parser.join();
                    });
                    bytes
                };
                pending_parse_len = 0;
                got_bytes
            } else {
                read_exact_at(
                    handle,
                    disk_offset + done_in_run as i64,
                    &mut read_buf[..want],
                )
            };

            if got == 0 {
                break;
            }
            let data = &read_buf[..got];
            let mut cursor = 0usize;

            if !carry.is_empty() {
                let need = record_size - carry.len();
                if data.len() >= need {
                    let rec_idx = (total_offset - carry.len() as u64) / record_size as u64;
                    carry.extend_from_slice(&data[..need]);
                    scratch.copy_from_slice(&carry);
                    match parse_record_in_place(
                        &mut scratch,
                        rec_idx,
                        bytes_per_sector,
                        bytes_per_cluster,
                        store_min_size,
                    ) {
                        ParsedRecord::Dir { index, node } => {
                            dirs.insert(index, node);
                        }
                        ParsedRecord::File { index, raw } => {
                            files.insert(index, raw);
                        }
                        _ => {}
                    }
                    carry.clear();
                    cursor = need;
                } else {
                    carry.extend_from_slice(data);
                    done_in_run += got as u64;
                    total_offset += got as u64;
                    continue;
                }
            }

            let aligned_len = ((data.len() - cursor) / record_size) * record_size;
            if aligned_len > 0 {
                parse_buf[..aligned_len].copy_from_slice(&data[cursor..cursor + aligned_len]);
                pending_parse_len = aligned_len;
                pending_parse_base_idx = (total_offset + cursor as u64) / record_size as u64;
                cursor += aligned_len;
            }

            if cursor < data.len() {
                carry.extend_from_slice(&data[cursor..]);
            }

            done_in_run += got as u64;
            total_offset += got as u64;
        }
    }

    if pending_parse_len > 0 {
        parse_chunk_parallel(
            &parse_buf[..pending_parse_len],
            pending_parse_base_idx,
            record_size,
            bytes_per_sector,
            bytes_per_cluster,
            store_min_size,
            &mut dirs,
            &mut files,
        );
    }

    if dirs.is_empty() && files.is_empty() {
        return None;
    }
    Some((dirs, files))
}

fn resolve_dir(
    frn: u64,
    dirs: &HashMap<u64, DirNode>,
    cache: &mut HashMap<u64, Option<String>>,
    drive: &str,
    skip_top_at_root: bool,
    skip_top: &[&str],
) -> Option<String> {
    if let Some(cached) = cache.get(&frn) {
        return cached.clone();
    }
    let root_idx = frn & 0xFFFF_FFFF_FFFF;
    if root_idx == 5 {
        let p = format!("{}\\", drive);
        cache.insert(frn, Some(p.clone()));
        return Some(p);
    }

    let mut chain: Vec<u64> = Vec::new();
    let mut cur_frn = frn;
    let mut base: Option<String> = None;

    loop {
        if let Some(cached) = cache.get(&cur_frn) {
            base = cached.clone();
            break;
        }
        let cur_idx = cur_frn & 0xFFFF_FFFF_FFFF;
        let cur_seq = (cur_frn >> 48) as u16;
        let Some(node) = dirs.get(&cur_idx) else {
            break;
        };
        if cur_seq != 0 && node.seq != cur_seq {
            break;
        }
        if node.reparse || node.name.starts_with('$') {
            break;
        }
        let parent_idx = node.parent_frn & 0xFFFF_FFFF_FFFF;
        if node.parent_frn == 0 || parent_idx == 5 {
            if skip_top_at_root {
                let lower = node.name.to_lowercase();
                if skip_top.iter().any(|s| *s == lower) {
                    break;
                }
            }
            let p = format!("{}\\{}", drive, node.name);
            cache.insert(cur_frn, Some(p.clone()));
            base = Some(p);
            break;
        }
        chain.push(cur_frn);
        cur_frn = node.parent_frn;
        if chain.len() > 4096 {
            break;
        }
    }

    let Some(mut path) = base else {
        for id in chain {
            cache.insert(id, None);
        }
        cache.insert(frn, None);
        return None;
    };

    for id in chain.iter().rev() {
        let idx = *id & 0xFFFF_FFFF_FFFF;
        let Some(node) = dirs.get(&idx) else {
            cache.insert(frn, None);
            return None;
        };
        path = format!("{}\\{}", path.trim_end_matches('\\'), node.name);
        cache.insert(*id, Some(path.clone()));
    }
    Some(path)
}
pub fn try_mft_scan(
    root: &str,
    min_size: u64,
    skip_top: &[&str],
    keep: KeepFn,
) -> Option<Vec<FileEntry>> {
    let trimmed = root.trim_end_matches('\\');
    let letter = trimmed.chars().next()?;
    if trimmed.len() < 2 || trimmed.as_bytes()[1] != b':' || !letter.is_ascii_alphabetic() {
        return None;
    }
    let drive = format!("{}:", letter.to_ascii_uppercase());
    let volume_path = format!("\\\\.\\{}", drive);
    let wpath = wide(&volume_path);
    let raw_handle = unsafe {
        CreateFileW(
            wpath.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if raw_handle.is_null() || raw_handle == INVALID_HANDLE_PTR {
        return None;
    }
    let handle = VolumeHandle(raw_handle);

    let mut volume = NtfsVolumeData::default();
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null_mut(),
            0,
            &mut volume as *mut _ as *mut c_void,
            std::mem::size_of::<NtfsVolumeData>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }

    let record_size = volume.bytes_per_file_record_segment as usize;
    let bytes_per_cluster = volume.bytes_per_cluster as usize;
    let bytes_per_sector = volume.bytes_per_sector as usize;
    if (record_size != 1024 && record_size != 4096)
        || bytes_per_cluster == 0
        || bytes_per_sector == 0
    {
        return None;
    }

    let serial = volume.volume_serial_number as u64;
    let usn_now = query_usn_journal(handle.0);
    let snap_file = snapshot_path(serial);
    let store_min_size = if min_size >= SNAPSHOT_FLOOR_MIN_SIZE {
        SNAPSHOT_FLOOR_MIN_SIZE
    } else {
        min_size
    };

    let mut snapshot_opt: Option<Arc<MftSnapshot>> = None;

    if let Some(journal) = usn_now {
        let cached = mem_snapshot_get(serial)
            .or_else(|| load_snapshot(&snap_file).map(Arc::new));
        if let Some(snap) = cached {
            if snap.volume_serial == serial
                && snap.journal_id == journal.usn_journal_id
                && snap.bytes_per_cluster == bytes_per_cluster as u32
                && snap.min_size <= min_size
            {
                if snap.next_usn == journal.next_usn {
                    mem_snapshot_put(serial, snap.clone());
                    snapshot_opt = Some(snap);
                } else if snap.next_usn >= journal.first_usn && snap.next_usn < journal.next_usn {
                    if let Some(changed) =
                        collect_usn_changed_indices(handle.0, &journal, snap.next_usn)
                    {
                        // 取出独占所有权再原地更新；仍被其他扫描持有时才退化为克隆。
                        drop(mem_snapshot_take(serial));
                        let mut snap = Arc::try_unwrap(snap).unwrap_or_else(|a| (*a).clone());
                        let mut out_buf = vec![0u8; 16 + record_size];
                        for idx in changed {
                            snap.dirs.remove(&idx);
                            snap.files.remove(&idx);
                            if let Some((actual_frn, _len)) =
                                read_single_mft_record(handle.0, idx, record_size, &mut out_buf)
                            {
                                if (actual_frn & 0xFFFF_FFFF_FFFF) == idx {
                                    let rec = &mut out_buf[12..12 + record_size];
                                    match parse_record_in_place(
                                        rec,
                                        idx,
                                        bytes_per_sector,
                                        bytes_per_cluster,
                                        snap.min_size,
                                    ) {
                                        ParsedRecord::Dir { index, node } => {
                                            snap.dirs.insert(index, node);
                                        }
                                        ParsedRecord::File { index, raw } => {
                                            snap.files.insert(index, raw);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        snap.next_usn = journal.next_usn;
                        let snap = Arc::new(snap);
                        mem_snapshot_put(serial, snap.clone());
                        save_snapshot_in_background(snap_file.clone(), snap.clone());
                        snapshot_opt = Some(snap);
                    }
                }
            }
        }
    }

    let snap: Arc<MftSnapshot> = match snapshot_opt {
        Some(s) => s,
        None => {
            let mut first = vec![0u8; record_size];
            if read_exact_at(
                handle.0,
                volume.mft_start_lcn * bytes_per_cluster as i64,
                &mut first,
            ) < record_size
            {
                return None;
            }
            let runs = match parse_record_in_place(
                &mut first,
                0,
                bytes_per_sector,
                bytes_per_cluster,
                store_min_size,
            ) {
                ParsedRecord::MftRoot { runs } => runs,
                _ => return None,
            };
            let (dirs, files) = full_mft_read_parallel(handle.0, &volume, runs, store_min_size)?;
            let snap = Arc::new(MftSnapshot {
                volume_serial: serial,
                journal_id: usn_now.map(|j| j.usn_journal_id).unwrap_or(0),
                next_usn: usn_now.map(|j| j.next_usn).unwrap_or(0),
                min_size: store_min_size,
                bytes_per_cluster: bytes_per_cluster as u32,
                dirs,
                files,
            });
            if usn_now.is_some() && store_min_size >= SNAPSHOT_FLOOR_MIN_SIZE {
                mem_snapshot_put(serial, snap.clone());
                save_snapshot_in_background(snap_file.clone(), snap.clone());
            }
            snap
        }
    };
    let dirs = &snap.dirs;
    let files = &snap.files;

    let is_drive_root = trimmed.len() == 2;
    let root_lower = trimmed.to_lowercase();
    let root_prefix = format!("{}\\", root_lower);
    let root_len = root_lower.len() + 1;
    let now_unix = crate::now_seconds();

    let mut dir_cache: HashMap<u64, Option<String>> = HashMap::with_capacity(dirs.len().min(65536));
    let mut entries = Vec::with_capacity(files.len());

    for raw in files.values() {
        if raw.size < min_size || !keep(&raw.name, raw.size) {
            continue;
        }
        let dir_path = if (raw.parent_frn & 0xFFFF_FFFF_FFFF) == 5 {
            format!("{}\\", drive)
        } else {
            let Some(p) = resolve_dir(
                raw.parent_frn,
                dirs,
                &mut dir_cache,
                &drive,
                is_drive_root,
                skip_top,
            ) else {
                continue;
            };
            p
        };

        let full_path = if dir_path.ends_with('\\') {
            format!("{}{}", dir_path, raw.name)
        } else {
            format!("{}\\{}", dir_path, raw.name)
        };

        if !is_drive_root {
            let lower = full_path.to_lowercase();
            if !lower.starts_with(&root_prefix) {
                continue;
            }
            if lower.len() > root_len {
                let first = lower[root_len..].split('\\').next().unwrap_or("");
                if skip_top.iter().any(|s| *s == first) {
                    continue;
                }
            }
        }

        // 近 5 分钟刚改过的文件可能还在系统脏页缓存里，走常规文件句柄避免读到旧盘面块。
        let recent = raw.mtime == 0 || now_unix.saturating_sub(raw.mtime) < RAW_READ_MIN_AGE_SECS;
        entries.push(FileEntry {
            path: full_path,
            size: raw.size,
            mtime: raw.mtime,
            sort_key: raw.sort_key,
            frn: raw.frn,
            first_lcn: raw.first_lcn,
            bytes_per_cluster: bytes_per_cluster as u32,
            can_raw_read: raw.can_raw_read && !recent,
        });
    }

    Some(entries)
}

pub fn fallback_walk_files(root: &str, min_size: u64, skip_top: &[&str]) -> Vec<FileEntry> {
    let root_path = Path::new(root);
    let Ok(entries) = std::fs::read_dir(root_path) else {
        return Vec::new();
    };
    let mut tops: Vec<PathBuf> = Vec::new();
    let mut out: Vec<FileEntry> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('$') {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.file_attributes() & SKIP_FILE_ATTRS != 0 {
            continue;
        }
        if meta.is_dir() {
            if skip_top.iter().any(|skip| skip.eq_ignore_ascii_case(&name)) {
                continue;
            }
            tops.push(entry.path());
        } else if meta.is_file() && meta.len() >= min_size {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            out.push(FileEntry {
                path: entry.path().to_string_lossy().to_string(),
                size: meta.len(),
                mtime,
                sort_key: 0,
                frn: 0,
                first_lcn: 0,
                bytes_per_cluster: 0,
                can_raw_read: false,
            });
        }
    }

    let use_parallel = crate::frag::is_ssd(root).unwrap_or(true);
    if use_parallel {
        let nested: Vec<FileEntry> = tops
            .into_par_iter()
            .flat_map(|dir| walk_dir_parallel(dir, min_size))
            .collect();
        out.extend(nested);
    } else {
        for dir in tops {
            walk_dir_seq(dir, min_size, &mut out);
        }
    }
    out
}

fn walk_dir_seq(start: PathBuf, min_size: u64, out: &mut Vec<FileEntry>) {
    let mut stack = vec![start];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('$') {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.file_attributes() & SKIP_FILE_ATTRS != 0 {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() && meta.len() >= min_size {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                out.push(FileEntry {
                    path: entry.path().to_string_lossy().to_string(),
                    size: meta.len(),
                    mtime,
                    sort_key: 0,
                    frn: 0,
                    first_lcn: 0,
                    bytes_per_cluster: 0,
                    can_raw_read: false,
                });
            }
        }
    }
}

fn walk_dir_parallel(dir: PathBuf, min_size: u64) -> Vec<FileEntry> {
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut subdirs: Vec<PathBuf> = Vec::new();
    let mut out: Vec<FileEntry> = Vec::new();

    for entry in read.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('$') {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.file_attributes() & SKIP_FILE_ATTRS != 0 {
            continue;
        }
        if meta.is_dir() {
            subdirs.push(entry.path());
        } else if meta.is_file() && meta.len() >= min_size {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            out.push(FileEntry {
                path: entry.path().to_string_lossy().to_string(),
                size: meta.len(),
                mtime,
                sort_key: 0,
                frn: 0,
                first_lcn: 0,
                bytes_per_cluster: 0,
                can_raw_read: false,
            });
        }
    }

    if !subdirs.is_empty() {
        let nested: Vec<FileEntry> = subdirs
            .into_par_iter()
            .flat_map(|sub| walk_dir_parallel(sub, min_size))
            .collect();
        out.extend(nested);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_windows_filetime_to_unix() {
        let unix_secs = 1_704_067_200u64;
        let ft = (unix_secs + 11_644_473_600) * 10_000_000;
        assert_eq!(filetime_to_unix(ft), unix_secs);
    }

    #[test]
    fn parses_first_lcn_and_clusters_from_runlist() {
        let buf = [0x31, 0x08, 0x45, 0x23, 0x01, 0x00];
        assert_eq!(parse_first_extent(&buf), (0x012345, 8));
        let sparse = [0x01, 0x08, 0x00];
        assert_eq!(parse_first_extent(&sparse), (0, 0));
    }

    #[test]
    fn fallback_walk_filters_size_and_skip_top() {
        let dir = std::env::temp_dir().join("cdc-mft-fallback-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("keep")).unwrap();
        std::fs::create_dir_all(dir.join("Windows")).unwrap();
        std::fs::create_dir_all(dir.join("$Skip")).unwrap();
        std::fs::write(dir.join("keep").join("big.bin"), vec![1u8; 2048]).unwrap();
        std::fs::write(dir.join("keep").join("small.bin"), vec![1u8; 100]).unwrap();
        std::fs::write(dir.join("Windows").join("sys.bin"), vec![1u8; 4096]).unwrap();
        std::fs::write(dir.join("$Skip").join("meta.bin"), vec![1u8; 4096]).unwrap();

        let found = fallback_walk_files(&dir.to_string_lossy(), 1024, &["windows"]);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("big.bin"));
        assert_eq!(found[0].size, 2048);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_files_where_drops_rejected_names_before_returning() {
        let dir = std::env::temp_dir().join("cdc-mft-keep-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("wanted.txt"), vec![1u8; 4096]).unwrap();
        std::fs::write(dir.join("sub").join("unwanted.bin"), vec![1u8; 4096]).unwrap();

        let seen_bad_name = std::sync::atomic::AtomicBool::new(false);
        let keep = |name: &str, _size: u64| {
            if name.contains('\\') || name.contains('/') {
                seen_bad_name.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            name.ends_with(".txt")
        };
        let found = scan_files_where(&dir.to_string_lossy(), 1024, &[], &keep);
        assert!(!seen_bad_name.load(std::sync::atomic::Ordering::Relaxed));
        assert!(found.iter().all(|e| e.path.ends_with("wanted.txt")));
        assert!(!found.iter().any(|e| e.path.ends_with("unwanted.bin")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_binary_roundtrip_preserves_dirs_and_files() {
        let dir = std::env::temp_dir().join("cdc-mft-snap-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let snap_file = dir.join("test.bin");

        let mut dirs = HashMap::new();
        dirs.insert(
            10,
            DirNode {
                seq: 3,
                parent_frn: 5,
                name: "Users".to_string(),
                reparse: false,
            },
        );
        let mut files = HashMap::new();
        files.insert(
            20,
            RawFile {
                frn: (1u64 << 48) | 20,
                parent_frn: (3u64 << 48) | 10,
                name: "video.mp4".to_string(),
                size: 4_194_304,
                mtime: 1_700_000_000,
                first_lcn: 999,
                sort_key: 999,
                can_raw_read: true,
            },
        );
        let snap = MftSnapshot {
            volume_serial: 0x1234_5678,
            journal_id: 0xABCD_EF01,
            next_usn: 42_000,
            min_size: 1 << 20,
            bytes_per_cluster: 4096,
            dirs,
            files,
        };
        save_snapshot(&snap_file, &snap).unwrap();

        let loaded = load_snapshot(&snap_file).unwrap();
        assert_eq!(loaded.volume_serial, 0x1234_5678);
        assert_eq!(loaded.journal_id, 0xABCD_EF01);
        assert_eq!(loaded.next_usn, 42_000);
        assert_eq!(loaded.bytes_per_cluster, 4096);
        assert_eq!(loaded.dirs.get(&10).unwrap().name, "Users");
        let f = loaded.files.get(&20).unwrap();
        assert_eq!(f.first_lcn, 999);
        assert!(f.can_raw_read);

        let _ = std::fs::remove_dir_all(&dir);
    }
}