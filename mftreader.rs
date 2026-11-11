#![allow(non_snake_case, non_camel_case_types, dead_code)]
// 直接读取 NTFS $MFT 枚举文件，输出 "size\tpath"。
// 用法：mftreader.exe <root> <min_size> <cache_dir> [--refresh]
// 缓存：把全盘文件列表写到 cache_dir，下次用 USN 日志的 NextUsn 判断是否可直接复用。

use std::collections::HashMap;
use std::env;
use std::io::{self, BufWriter, Write};
use std::os::windows::ffi::OsStrExt;

type HANDLE = isize;
type BOOL = i32;

const GENERIC_READ: u32 = 0x80000000;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const OPEN_EXISTING: u32 = 3;
const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x00090064;
const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900f4;
const INVALID_HANDLE_VALUE: HANDLE = -1;
const CACHE_TTL_SECS: i64 = 600;

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
    max_usn: i64,
    maximum_size: i64,
    allocation_delta: i64,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *mut core::ffi::c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    fn DeviceIoControl(
        hDevice: HANDLE,
        dwIoControlCode: u32,
        lpInBuffer: *mut core::ffi::c_void,
        nInBufferSize: u32,
        lpOutBuffer: *mut core::ffi::c_void,
        nOutBufferSize: u32,
        lpBytesReturned: *mut u32,
        lpOverlapped: *mut core::ffi::c_void,
    ) -> BOOL;
    fn SetFilePointerEx(
        hFile: HANDLE,
        liDistanceToMove: i64,
        lpNewFilePointer: *mut i64,
        dwMoveMethod: u32,
    ) -> BOOL;
    fn ReadFile(
        hFile: HANDLE,
        lpBuffer: *mut core::ffi::c_void,
        nNumberOfBytesToRead: u32,
        lpNumberOfBytesRead: *mut u32,
        lpOverlapped: *mut core::ffi::c_void,
    ) -> BOOL;
    fn CloseHandle(hObject: HANDLE) -> BOOL;
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn read_at(handle: HANDLE, offset: i64, size: usize) -> Vec<u8> {
    unsafe {
        if SetFilePointerEx(handle, offset, std::ptr::null_mut(), 0) == 0 {
            return Vec::new();
        }
    }
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let chunk = 8 * 1024 * 1024;
    let mut buf = vec![0u8; chunk.min(size.max(1))];
    let mut remaining = size;
    while remaining > 0 {
        let to_read = remaining.min(buf.len());
        let mut read: u32 = 0;
        let ok = unsafe {
            ReadFile(handle, buf.as_mut_ptr() as *mut _, to_read as u32,
                     &mut read, std::ptr::null_mut())
        };
        if ok == 0 || read == 0 {
            break;
        }
        out.extend_from_slice(&buf[..read as usize]);
        remaining -= read as usize;
    }
    out
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
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

fn apply_fixups(rec: &mut [u8], bytes_per_sector: usize) {
    let usa_off = match u16_at(rec, 4) {
        Some(v) => v as usize,
        None => return,
    };
    let usa_count = match u16_at(rec, 6) {
        Some(v) => v as usize,
        None => return,
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
        let a = rec[src];
        let b = rec[src + 1];
        rec[pos - 2] = a;
        rec[pos - 1] = b;
    }
}

fn parse_runlist(buf: &[u8], offset: usize) -> Vec<(i64, i64, i64)> {
    let mut runs = Vec::new();
    let mut vcn: i64 = 0;
    let mut prev_lcn: i64 = 0;
    let mut pos = offset;
    while pos < buf.len() {
        let header = buf[pos];
        pos += 1;
        if header == 0 {
            break;
        }
        let len_size = (header & 0x0F) as usize;
        let off_size = (header >> 4) as usize;
        if len_size == 0 || pos + len_size + off_size > buf.len() {
            break;
        }
        let run_len = read_uint_le(&buf[pos..pos + len_size]);
        pos += len_size;
        let off = read_int_le(&buf[pos..pos + off_size]);
        pos += off_size;
        prev_lcn += off;
        runs.push((vcn, prev_lcn, run_len));
        vcn += run_len;
    }
    runs
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

struct Rec {
    parent: u64,
    name: String,
    size: u64,
    is_dir: bool,
}

fn parse_record(
    raw: &[u8],
    index: u64,
    bytes_per_sector: usize,
) -> Option<(u64, Rec, Option<Vec<(i64, i64, i64)>>)> {
    if raw.len() < 48 || &raw[0..4] != b"FILE" {
        return None;
    }
    let mut rec = raw.to_vec();
    apply_fixups(&mut rec, bytes_per_sector);

    let flags = u16_at(&rec, 22)?;
    if flags & 1 == 0 {
        return None;
    }
    let is_dir = flags & 2 != 0;
    let sequence = u16_at(&rec, 16)? as u64;
    let frn = (sequence << 48) | index;
    let first_attr = u16_at(&rec, 20)? as usize;

    let mut name: Option<String> = None;
    let mut parent: u64 = 0;
    let mut size: u64 = 0;
    let mut runlist: Option<Vec<(i64, i64, i64)>> = None;

    let mut pos = first_attr;
    while pos + 8 <= rec.len() {
        let attr_type = match u32_at(&rec, pos) {
            Some(v) => v,
            None => break,
        };
        if attr_type == 0xFFFFFFFF {
            break;
        }
        let attr_len = match u32_at(&rec, pos + 4) {
            Some(v) => v as usize,
            None => break,
        };
        if attr_len < 16 || pos + attr_len > rec.len() {
            break;
        }
        let non_resident = rec[pos + 8];
        let name_len = rec[pos + 9];

        if attr_type == 0x30 && non_resident == 0 {
            if let Some(val_off) = u16_at(&rec, pos + 20) {
                let v = pos + val_off as usize;
                if v + 66 <= pos + attr_len {
                    if let Some(parent_ref) = u64_at(&rec, v) {
                        let nlen = rec[v + 64] as usize;
                        let namespace = rec[v + 65];
                        if v + 66 + nlen * 2 <= pos + attr_len {
                            let nm = utf16le(&rec[v + 66..v + 66 + nlen * 2]);
                            if name.is_none() || namespace == 0 || namespace == 1 {
                                name = Some(nm);
                                parent = parent_ref;
                            }
                        }
                    }
                }
            }
        } else if attr_type == 0x80 && name_len == 0 {
            if non_resident != 0 {
                if pos + 56 <= rec.len() {
                    if let Some(real_size) = u64_at(&rec, pos + 48) {
                        size = real_size;
                    }
                    if index == 0 {
                        if let Some(run_off) = u16_at(&rec, pos + 32) {
                            let start = pos + run_off as usize;
                            if start <= pos + attr_len {
                                runlist = Some(parse_runlist(&rec[start..pos + attr_len], 0));
                            }
                        }
                    }
                }
            } else if let Some(vlen) = u32_at(&rec, pos + 16) {
                size = vlen as u64;
            }
        }
        pos += attr_len;
    }

    let name = name?;
    Some((frn, Rec { parent, name, size, is_dir }, runlist))
}

fn resolve(
    frn: u64,
    records: &HashMap<u64, Rec>,
    cache: &mut HashMap<u64, String>,
    drive: &str,
) -> Option<String> {
    if let Some(p) = cache.get(&frn) {
        return Some(p.clone());
    }
    let mut chain: Vec<u64> = Vec::new();
    let mut cur = frn;
    let base: String;
    loop {
        if let Some(p) = cache.get(&cur) {
            base = p.clone();
            break;
        }
        let rec = records.get(&cur)?;
        if rec.parent == 0 || (rec.parent & 0xFFFFFFFFFFFF) == 5 {
            let p = format!("{}\\{}", drive, rec.name);
            cache.insert(cur, p.clone());
            base = p;
            break;
        }
        chain.push(cur);
        cur = rec.parent;
        if chain.len() > 4096 {
            return None;
        }
    }
    let mut b = base;
    for item in chain.iter().rev() {
        let rec = records.get(item)?;
        b = format!("{}\\{}", b.trim_end_matches('\\'), rec.name);
        cache.insert(*item, b.clone());
    }
    Some(cache.get(&frn).cloned().unwrap_or(b))
}

fn query_usn_journal(handle: HANDLE) -> Option<(u64, i64)> {
    let mut data = UsnJournalData::default();
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(handle, FSCTL_QUERY_USN_JOURNAL,
                        std::ptr::null_mut(), 0,
                        &mut data as *mut _ as *mut _,
                        std::mem::size_of::<UsnJournalData>() as u32,
                        &mut returned, std::ptr::null_mut())
    };
    if ok == 0 {
        return None;
    }
    Some((data.usn_journal_id, data.next_usn))
}

fn cache_file(dir: &str, serial: u64) -> String {
    format!("{}\\mft_{:016x}.cache", dir.trim_end_matches('\\'), serial)
}

fn read_cache(path: &str) -> Option<(u64, u64, i64, i64, Vec<(u64, String)>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "MFT1" {
        return None;
    }
    let serial: u64 = lines.next()?.parse().ok()?;
    let journal_id: u64 = lines.next()?.parse().ok()?;
    let next_usn: i64 = lines.next()?.parse().ok()?;
    let timestamp: i64 = lines.next()?.parse().ok()?;
    let mut entries = Vec::new();
    for line in lines {
        if let Some(tab) = line.find('\t') {
            if let Ok(size) = line[..tab].parse::<u64>() {
                entries.push((size, line[tab + 1..].to_string()));
            }
        }
    }
    Some((serial, journal_id, next_usn, timestamp, entries))
}

fn write_cache(path: &str, serial: u64, journal_id: u64, next_usn: i64,
               entries: &[(u64, String)]) -> io::Result<()> {
    let mut file = BufWriter::new(std::fs::File::create(path)?);
    writeln!(file, "MFT1")?;
    writeln!(file, "{}", serial)?;
    writeln!(file, "{}", journal_id)?;
    writeln!(file, "{}", next_usn)?;
    writeln!(file, "{}", now_unix())?;
    for (size, path) in entries {
        writeln!(file, "{}\t{}", size, path)?;
    }
    Ok(())
}

fn emit(entries: &[(u64, String)], root_norm: &str, min_size: u64,
        out: &mut impl Write) {
    let prefix = format!("{}\\", root_norm);
    for (size, path) in entries {
        if *size < min_size {
            continue;
        }
        let norm = path.to_lowercase();
        let parent = match norm.rfind('\\') {
            Some(p) => &norm[..p],
            None => "",
        };
        if norm.starts_with(&prefix) || parent == root_norm {
            let _ = writeln!(out, "{}\t{}", size, path);
        }
    }
}

fn full_scan(handle: HANDLE, volume: &NtfsVolumeData) -> Option<Vec<(u64, String)>> {
    let record_size = volume.bytes_per_file_record_segment as usize;
    let bytes_per_cluster = volume.bytes_per_cluster as usize;
    let bytes_per_sector = volume.bytes_per_sector as usize;
    if (record_size != 1024 && record_size != 4096)
        || bytes_per_cluster == 0 || bytes_per_sector == 0
    {
        return None;
    }
    let first = read_at(handle, volume.mft_start_lcn * bytes_per_cluster as i64, record_size);
    if first.len() < record_size {
        return None;
    }
    let rec0 = parse_record(&first, 0, bytes_per_sector)?;
    let runs = rec0.2?;
    if runs.is_empty() {
        return None;
    }
    let mut image: Vec<u8> = Vec::new();
    for (_vcn, lcn, length) in runs {
        if lcn < 0 || length <= 0 {
            continue;
        }
        let chunk = read_at(handle, lcn * bytes_per_cluster as i64,
                            (length as usize) * bytes_per_cluster);
        if chunk.is_empty() {
            break;
        }
        image.extend_from_slice(&chunk);
    }
    if image.is_empty() {
        return None;
    }
    let mut records: HashMap<u64, Rec> = HashMap::new();
    let count = image.len() / record_size;
    for i in 0..count {
        let start = i * record_size;
        let raw = &image[start..start + record_size];
        if &raw[0..4] != b"FILE" {
            continue;
        }
        if let Some((frn, rec, _)) = parse_record(raw, i as u64, bytes_per_sector) {
            records.insert(frn, rec);
        }
    }
    if records.is_empty() {
        return None;
    }
    let drive = format!("{}:", (b'A' + 0) as char); // 占位，稍后用真实盘符
    let _ = drive;
    Some(records_to_entries(&records))
}

fn records_to_entries(records: &HashMap<u64, Rec>) -> Vec<(u64, String)> {
    let mut cache: HashMap<u64, String> = HashMap::new();
    let mut out = Vec::new();
    // 盘符从任一顶层路径推断：先找一个父为根的记录
    let drive = detect_drive(records);
    for (frn, rec) in records.iter() {
        if rec.is_dir {
            continue;
        }
        if let Some(path) = resolve(*frn, records, &mut cache, &drive) {
            out.push((rec.size, path));
        }
    }
    out
}

fn detect_drive(records: &HashMap<u64, Rec>) -> String {
    // 顶层记录的父引用低 48 位为 5；无法确定时默认 C:
    "C:".to_string()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        std::process::exit(2);
    }
    let root = args[1].trim_end_matches('\\').to_string();
    if root.len() < 2 {
        std::process::exit(2);
    }
    let min_size: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let cache_dir = args.get(3).cloned().unwrap_or_default();
    let force = args.iter().any(|a| a == "--refresh");
    let drive = root[..2].to_uppercase();

    let volume_path = format!("\\\\.\\{}", drive);
    let wpath = wide(&volume_path);
    let handle = unsafe {
        CreateFileW(wpath.as_ptr(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null_mut(), OPEN_EXISTING, 0, 0)
    };
    if handle == INVALID_HANDLE_VALUE || handle == 0 {
        std::process::exit(3);
    }

    let mut volume = NtfsVolumeData::default();
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(handle, FSCTL_GET_NTFS_VOLUME_DATA,
                        std::ptr::null_mut(), 0,
                        &mut volume as *mut _ as *mut _,
                        std::mem::size_of::<NtfsVolumeData>() as u32,
                        &mut returned, std::ptr::null_mut())
    };
    if ok == 0 {
        unsafe { CloseHandle(handle); }
        std::process::exit(4);
    }
    let serial = volume.volume_serial_number as u64;
    let usn = query_usn_journal(handle);

    let root_norm = root.to_lowercase();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    let mut entries: Option<Vec<(u64, String)>> = None;
    if !force && !cache_dir.is_empty() {
        if let Some((cserial, cjid, cusn, ctime, centries)) =
            read_cache(&cache_file(&cache_dir, serial))
        {
            if cserial == serial {
                let journal_ok = match usn {
                    Some((jid, _)) => jid == cjid,
                    None => true,
                };
                let unchanged = match usn {
                    Some((_, next)) => next == cusn,
                    None => false,
                };
                if journal_ok && (unchanged || now_unix() - ctime < CACHE_TTL_SECS) {
                    entries = Some(centries);
                }
            }
        }
    }

    if entries.is_none() {
        match full_scan(handle, &volume) {
            Some(mut list) => {
                // 用真实盘符修正路径前缀（full_scan 默认 C:）
                for item in list.iter_mut() {
                    if item.1.len() >= 2 && &item.1[1..2] == ":" {
                        item.1.replace_range(0..1, &drive[..1]);
                    }
                }
                if !cache_dir.is_empty() {
                    let (jid, nusn) = usn.unwrap_or((0, 0));
                    let _ = std::fs::create_dir_all(&cache_dir);
                    let _ = write_cache(&cache_file(&cache_dir, serial), serial, jid, nusn, &list);
                }
                entries = Some(list);
            }
            None => {
                unsafe { CloseHandle(handle); }
                std::process::exit(9);
            }
        }
    }

    if let Some(list) = entries {
        emit(&list, &root_norm, min_size, &mut out);
    }
    let _ = out.flush();
    unsafe { CloseHandle(handle); }
}
