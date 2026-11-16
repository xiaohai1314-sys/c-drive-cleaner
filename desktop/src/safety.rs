//! 路径安全与删除，对应 Python 版 safepath.py 的核心判断。
//!
//! 原则：宁可跳过，不可误删。所有删除前都过一遍永不清理名单、受保护路径、
//! 用户文档库、密码库和重解析点检查；删除时按句柄操作，避免父目录在检查后
//! 被换成链接。

use glob::Pattern;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const DELETE_ACCESS: u32 = 0x0001_0000;
const FILE_DISPOSITION_INFO_CLASS: i32 = 4;
const FILE_BASIC_INFO_CLASS: i32 = 0;
const FILE_RENAME_INFO_CLASS: i32 = 10;
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
const FILE_READ_DATA: u32 = 0x0000_0001;
const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
const FILE_WRITE_DATA: u32 = 0x0000_0002;
const FILE_ADD_FILE: u32 = 0x0000_0002;
const FILE_ADD_SUBDIRECTORY: u32 = 0x0000_0004;
const FILE_TRAVERSE: u32 = 0x0000_0020;
const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const FILE_SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | 0x0000_0004;
const FILE_OPEN: u32 = 1;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;

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
    "documents",
    "desktop",
    "downloads",
    "my documents",
    "文档",
    "桌面",
    "下载",
    "pictures",
    "music",
    "videos",
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
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn GetFileInformationByHandleEx(
        handle: *mut c_void,
        class: i32,
        info: *mut c_void,
        size: u32,
    ) -> i32;
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: *mut c_void,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_quality_of_service: *mut c_void,
}

#[repr(C)]
union IoStatusValue {
    status: i32,
    pointer: *mut c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: IoStatusValue,
    information: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FileBasicInfo {
    creation_time: i64,
    last_access_time: i64,
    last_write_time: i64,
    change_time: i64,
    file_attributes: u32,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtCreateFile(
        file_handle: *mut *mut c_void,
        desired_access: u32,
        object_attributes: *mut ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        allocation_size: *mut i64,
        file_attributes: u32,
        share_access: u32,
        create_disposition: u32,
        create_options: u32,
        ea_buffer: *mut c_void,
        ea_length: u32,
    ) -> i32;
    fn NtSetInformationFile(
        file_handle: *mut c_void,
        io_status_block: *mut IoStatusBlock,
        file_information: *mut c_void,
        length: u32,
        information_class: i32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

pub enum DeleteOutcome {
    Deleted,
    Skipped,
    /// Retained for callers compiled against the prior result model; the
    /// handle-relative implementation does not schedule path-based deletion.
    #[allow(dead_code)]
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
        return format!("{}{}", drive, rest)
            .trim_end_matches('\\')
            .to_string();
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
        || (norm.len() == 3
            && norm.as_bytes()[1] == b':'
            && (norm.ends_with('\\') || norm.ends_with('/')))
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
    for name in [
        "SystemRoot",
        "WINDIR",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
    ] {
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
        &[
            "\\windows",
            "\\system volume information",
            "\\recovery",
            "\\boot",
            "\\$recycle.bin",
        ],
    ) {
        return true;
    }
    let target = normalize(path)
        .to_lowercase()
        .trim_end_matches(['\\', '/'])
        .to_string();
    for key in ["SystemRoot", "WINDIR"] {
        if let Ok(value) = std::env::var(key) {
            let root = normalize(&value)
                .to_lowercase()
                .trim_end_matches(['\\', '/'])
                .to_string();
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
        &[
            "\\program files",
            "\\program files (x86)",
            "\\programdata",
            "\\perflogs",
        ],
    ) {
        return true;
    }
    let target = normalize(path)
        .to_lowercase()
        .trim_end_matches(['\\', '/'])
        .to_string();
    for root in env_roots() {
        let root = root.trim_end_matches(['\\', '/']);
        if !root.is_empty() && (target == root || target.starts_with(&format!("{}\\", root))) {
            return true;
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if target
            == normalize(&profile)
                .to_lowercase()
                .trim_end_matches(['\\', '/'])
        {
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
    let names = [
        "Documents",
        "Desktop",
        "Downloads",
        "My Documents",
        "文档",
        "桌面",
        "下载",
        "Pictures",
        "Music",
        "Videos",
    ];
    let mut roots = Vec::new();
    for base in bases {
        for name in names {
            roots.push(
                normalize(&format!("{}\\{}", base.trim_end_matches('\\'), name)).to_lowercase(),
            );
        }
    }
    roots
}

pub fn is_user_library(path: &str) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    let target = normalize(path)
        .to_lowercase()
        .trim_end_matches('\\')
        .to_string();
    for root in user_library_roots() {
        let root = root.trim_end_matches('\\');
        if target == root || target.starts_with(&format!("{}\\", root)) {
            return true;
        }
    }
    let text = tail(path);
    let parts: Vec<&str> = text
        .trim_start_matches('\\')
        .split('\\')
        .filter(|p| !p.is_empty())
        .collect();
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
    let target = normalize(path)
        .to_lowercase()
        .trim_end_matches(['\\', '/'])
        .to_string();
    for root in roots {
        let base = normalize(root)
            .to_lowercase()
            .trim_end_matches(['\\', '/'])
            .to_string();
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
    reparse_point_status(path).unwrap_or(false)
}

/// 检查重解析点并保留元数据错误；删除/恢复路径应把无法检查视为不安全。
pub fn reparse_point_status(path: &str) -> std::io::Result<bool> {
    let meta = std::fs::symlink_metadata(path)?;
    Ok(meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || meta.file_type().is_symlink())
}

fn win32_path_parts(path: &Path) -> io::Result<(PathBuf, Vec<String>)> {
    let raw = path.to_string_lossy().replace('/', "\\");
    if raw.len() < 3
        || !raw.as_bytes()[0].is_ascii_alphabetic()
        || raw.as_bytes()[1] != b':'
        || raw.as_bytes()[2] != b'\\'
        || path_syntax_error(&raw)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a normalized absolute drive path",
        ));
    }
    let normalized = normalize(&raw);
    let root = PathBuf::from(&normalized[..3]);
    let parts = normalized[3..]
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    Ok((root, parts))
}

fn nt_error(status: i32) -> io::Error {
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}

fn open_drive_root_with_access(path: &Path, access: u32) -> io::Result<File> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_ALL,
            std::ptr::null_mut(),
            3, // OPEN_EXISTING
            0x0200_0000 | 0x0020_0000,
            std::ptr::null_mut(),
        )
    };
    if handle as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "drive root is not a plain directory",
        ));
    }
    Ok(file)
}

fn open_relative(
    parent: &File,
    name: &str,
    access: u32,
    options: u32,
    disposition: u32,
    share: u32,
) -> io::Result<File> {
    if name.is_empty()
        || name.contains(['\\', '/', ':'])
        || name == "."
        || name == ".."
        || name.contains('\0')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid relative name",
        ));
    }
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    let byte_len = wide
        .len()
        .checked_mul(2)
        .filter(|len| *len <= u16::MAX as usize - 2)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component too long"))?;
    wide.push(0);
    let mut object_name = UnicodeString {
        length: byte_len as u16,
        maximum_length: (byte_len + 2) as u16,
        buffer: wide.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle() as *mut c_void,
        object_name: &mut object_name,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut status_block = IoStatusBlock {
        status: IoStatusValue { status: 0 },
        information: 0,
    };
    let mut handle = std::ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &mut attributes,
            &mut status_block,
            std::ptr::null_mut(),
            0,
            share,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null_mut(),
            0,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Open every directory component relative to the already-open parent.  Handles
/// are retained as a chain; intermediate directories are opened without
/// FILE_SHARE_DELETE so their names cannot be swapped while the final operation
/// is performed.  A reparse point in any component is rejected on its handle.
fn open_directory_chain(path: &Path) -> io::Result<Vec<File>> {
    open_directory_chain_with_final_access(path, 0)
}

/// Open ancestors with traversal/read access and grant extra rights only to the
/// final directory handle, which is the actual parent of the relative operation.
fn open_directory_chain_with_final_access(path: &Path, final_access: u32) -> io::Result<Vec<File>> {
    let (root, components) = win32_path_parts(path)?;
    let base_access = FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE_ACCESS;
    let root_access = base_access
        | if components.is_empty() {
            final_access
        } else {
            0
        };
    let root_handle = open_drive_root_with_access(&root, root_access)?;
    let mut chain = vec![root_handle];
    for (index, component) in components.iter().enumerate() {
        let parent = chain.last().expect("root handle is always present");
        let access = base_access
            | if index + 1 == components.len() {
                final_access
            } else {
                0
            };
        let child = open_relative(
            parent,
            component,
            access,
            FILE_DIRECTORY_FILE,
            FILE_OPEN,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )?;
        let metadata = child.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "reparse point or non-directory in parent path",
            ));
        }
        chain.push(child);
    }
    Ok(chain)
}

fn open_parent_chain(path: &Path, roots: &[String]) -> io::Result<(Vec<File>, String)> {
    open_parent_chain_with_rights(path, roots, 0)
}

fn open_parent_chain_for_create(
    path: &Path,
    roots: &[String],
    parent_rights: u32,
) -> io::Result<(Vec<File>, String)> {
    open_parent_chain_with_rights(path, roots, parent_rights)
}

fn open_parent_chain_with_rights(
    path: &Path,
    roots: &[String],
    parent_rights: u32,
) -> io::Result<(Vec<File>, String)> {
    let path_text = path.to_string_lossy().to_string();
    if !is_within(&path_text, roots) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path is outside the allowed roots",
        ));
    }
    let name = path
        .file_name()
        .and_then(|part| part.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"))?
        .to_string();
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let chain = open_directory_chain_with_final_access(parent, parent_rights)?;
    Ok((chain, name))
}

/// Open and pin a plain directory below one of the caller's allowed roots.
pub fn pin_directory_checked(path: &Path, roots: &[String]) -> io::Result<Vec<File>> {
    let text = path.to_string_lossy().to_string();
    if path_syntax_error(&text) || !is_within(&text, roots) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory is outside the allowed roots or has invalid syntax",
        ));
    }
    open_directory_chain(path)
}

/// Safely create a new directory tree. Existing ancestors are opened without
/// FILE_SHARE_DELETE; missing ancestors are created relative to their pinned
/// parent. The final component must not already exist. Keep the returned chain
/// alive while using the new tree to prevent path replacement.
pub fn create_directory_tree_checked(path: &Path) -> io::Result<Vec<File>> {
    let text = path.to_string_lossy().to_string();
    if path_syntax_error(&text) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid directory path",
        ));
    }
    let (root, components) = win32_path_parts(path)?;
    if components.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a volume root cannot be used as a new migration target",
        ));
    }
    // No DELETE access is needed to pin these names: withholding FILE_SHARE_DELETE
    // blocks replacement while allowing later checked handles to open the same directories.
    let base_access = FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE_ACCESS;
    let root_handle = open_drive_root_with_access(&root, base_access)?;
    let mut chain = vec![root_handle];
    let mut parent_path = root.clone();
    for (index, component) in components.iter().enumerate() {
        let final_component = index + 1 == components.len();
        let parent = chain.last().expect("root handle is always present");
        let child = if final_component {
            let create_parent =
                open_directory_chain_with_final_access(&parent_path, FILE_ADD_SUBDIRECTORY)?;
            open_relative(
                create_parent.last().expect("root handle is always present"),
                component,
                base_access,
                FILE_DIRECTORY_FILE,
                2, // FILE_CREATE: fail if the destination already exists
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            )?
        } else {
            match open_relative(
                parent,
                component,
                base_access,
                FILE_DIRECTORY_FILE,
                FILE_OPEN,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            ) {
                Ok(existing) => existing,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let create_parent = open_directory_chain_with_final_access(
                        &parent_path,
                        FILE_ADD_SUBDIRECTORY,
                    )?;
                    open_relative(
                        create_parent.last().expect("root handle is always present"),
                        component,
                        base_access,
                        FILE_DIRECTORY_FILE,
                        2, // FILE_CREATE: fail if another entry appeared
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                    )?
                }
                Err(error) => return Err(error),
            }
        };
        let metadata = child.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "directory component is a reparse point or non-directory",
            ));
        }
        chain.push(child);
        parent_path.push(component);
    }
    Ok(chain)
}

/// Create one new directory relative to a checked parent below `roots`.
/// The returned parent chain and directory handle should remain alive while
/// traversing or writing into the new directory. The handle omits DELETE access;
/// withholding FILE_SHARE_DELETE alone keeps the new directory name pinned.
pub fn create_directory_checked(path: &Path, roots: &[String]) -> io::Result<(Vec<File>, File)> {
    let (chain, name) = open_parent_chain_for_create(path, roots, FILE_ADD_SUBDIRECTORY)?;
    let parent = chain.last().expect("root handle is always present");
    let directory = open_relative(
        parent,
        &name,
        FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE_ACCESS,
        FILE_DIRECTORY_FILE,
        2, // FILE_CREATE: fail if any entry already exists
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )?;
    let metadata = directory.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "created directory is not an ordinary directory",
        ));
    }
    Ok((chain, directory))
}

/// Create a new ordinary file relative to a checked parent directory handle.
/// The caller keeps the returned handle chain alive through the complete write.
pub fn create_new_file_checked(path: &Path, roots: &[String]) -> io::Result<(Vec<File>, File)> {
    let (chain, name) = open_parent_chain_for_create(path, roots, FILE_ADD_FILE)?;
    let parent = chain.last().expect("root handle is always present");
    let file = open_relative(
        parent,
        &name,
        FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS,
        FILE_NON_DIRECTORY_FILE,
        2, // FILE_CREATE: fail if any entry already exists
        FILE_SHARE_READ,
    )?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "created file is not an ordinary file",
        ));
    }
    Ok((chain, file))
}

/// Open an ordinary file for reading relative to a checked parent handle.
pub fn open_read_file_checked(path: &Path, roots: &[String]) -> io::Result<(Vec<File>, File)> {
    let (chain, name) = open_parent_chain(path, roots)?;
    let parent = chain.last().expect("root handle is always present");
    let file = open_relative(
        parent,
        &name,
        FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS,
        FILE_NON_DIRECTORY_FILE,
        FILE_OPEN,
        FILE_SHARE_READ,
    )?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "opened file is not an ordinary file",
        ));
    }
    Ok((chain, file))
}

fn open_plain_file(path: &Path, roots: &[String]) -> io::Result<(Vec<File>, File)> {
    let (chain, name) = open_parent_chain(path, roots)?;
    let parent = chain.last().expect("root handle is always present");
    let file = open_relative(
        parent,
        &name,
        DELETE_ACCESS | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE_ACCESS,
        FILE_NON_DIRECTORY_FILE,
        FILE_OPEN,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "target is a reparse point or directory",
        ));
    }
    Ok((chain, file))
}

fn clear_readonly_and_delete(file: &File) -> bool {
    // GetFileInformationByHandleEx fills the full basic-info record. Keep its
    // original timestamps and change only READONLY before setting it back.
    let mut info = FileBasicInfo {
        creation_time: 0,
        last_access_time: 0,
        last_write_time: 0,
        change_time: 0,
        file_attributes: 0,
    };
    let got_info = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as *mut c_void,
            FILE_BASIC_INFO_CLASS,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<FileBasicInfo>() as u32,
        )
    };
    if got_info == 0 || info.file_attributes & FILE_ATTRIBUTE_READONLY == 0 {
        return false;
    }
    info.file_attributes &= !FILE_ATTRIBUTE_READONLY;
    let set_info = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as *mut c_void,
            FILE_BASIC_INFO_CLASS,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<FileBasicInfo>() as u32,
        )
    };
    set_info != 0 && mark_delete(file)
}

/// Remove a regular file relative to verified, pinned parent directory handles.
fn delete_plain_file(path: &Path, roots: &[String]) -> io::Result<bool> {
    let (_chain, file) = open_plain_file(path, roots)?;
    if mark_delete(&file) || clear_readonly_and_delete(&file) {
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Remove an empty ordinary directory using its checked parent handle.
pub fn remove_directory_checked(path: &Path, roots: &[String]) -> io::Result<bool> {
    let (chain, name) = open_parent_chain(path, roots)?;
    let parent = chain.last().expect("root handle is always present");
    let directory = open_relative(
        parent,
        &name,
        DELETE_ACCESS | FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS,
        FILE_DIRECTORY_FILE,
        FILE_OPEN,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )?;
    let metadata = directory.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
        return Ok(false);
    }
    let deleted = mark_delete(&directory);
    drop(directory);
    drop(chain);
    Ok(deleted)
}

/// Recursively remove a migration backup without following or path-deleting through
/// reparse points.  Any reparse child is deliberately retained; a partial cleanup
/// is safer than crossing a link or mount point.
pub fn remove_tree_checked(root: &Path, allowed_roots: &[String]) -> io::Result<u64> {
    fn remove_contents(dir: &Path, roots: &[String], removed: &mut u64) -> io::Result<()> {
        let chain = open_directory_chain(dir)?;
        let metadata = chain
            .last()
            .expect("root handle is always present")
            .metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "tree root is not a plain directory",
            ));
        }
        // The pinned chain is opened without FILE_SHARE_DELETE, preventing its
        // path components from being renamed while this path-based enumeration
        // runs. Every destructive operation still reopens the parent chain
        // handle-relatively and fails closed if a component is a reparse point.
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let child = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                remove_contents(&child, roots, removed)?;
                if remove_directory_checked(&child, roots)? {
                    *removed += 1;
                }
            } else if file_type.is_file() && delete_plain_file(&child, roots)? {
                *removed += 1;
            }
        }
        drop(chain);
        Ok(())
    }

    if !is_within(&root.to_string_lossy(), allowed_roots) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "tree root is outside the allowed roots",
        ));
    }
    let mut removed = 0;
    remove_contents(root, allowed_roots, &mut removed)?;
    if !remove_directory_checked(root, allowed_roots)? {
        return Err(io::Error::new(
            io::ErrorKind::DirectoryNotEmpty,
            "tree could not be fully removed",
        ));
    }
    Ok(removed + 1)
}

/// Rename a plain directory to a sibling name, relative to the verified parent handle.
/// At least one endpoint must be in an explicitly allowed migration root.
pub fn rename_directory_sibling_checked(
    from: &Path,
    to: &Path,
    allowed_roots: &[String],
) -> io::Result<()> {
    let from_text = from.to_string_lossy().to_string();
    let to_text = to.to_string_lossy().to_string();
    if path_syntax_error(&from_text)
        || path_syntax_error(&to_text)
        || (!is_within(&from_text, allowed_roots) && !is_within(&to_text, allowed_roots))
        || from.parent().map(normalize_path).as_deref()
            != to.parent().map(normalize_path).as_deref()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "rename endpoints are not permitted siblings",
        ));
    }
    let from_name = from
        .file_name()
        .and_then(|part| part.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid source name"))?;
    let to_name = to
        .file_name()
        .and_then(|part| part.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid target name"))?;
    let parent = from
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"))?;
    let chain = open_directory_chain_with_final_access(parent, FILE_ADD_SUBDIRECTORY)?;
    let parent_handle = chain.last().expect("root handle is always present");
    let source = open_relative(
        parent_handle,
        from_name,
        DELETE_ACCESS | FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS,
        FILE_DIRECTORY_FILE,
        FILE_OPEN,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )?;
    let source_meta = source.metadata()?;
    if source_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !source_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "rename source is not a plain directory",
        ));
    }
    match open_relative(
        parent_handle,
        to_name,
        FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS,
        0,
        FILE_OPEN,
        FILE_SHARE_ALL,
    ) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "rename target exists",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // FILE_RENAME_INFORMATION has a variable-length WCHAR tail.  The offsets follow
    // the native C layout and avoid a path-based rename after validation.
    #[repr(C)]
    struct RenamePrefix {
        replace_if_exists: u8,
        root_directory: *mut c_void,
        file_name_length: u32,
    }
    let wide: Vec<u16> = to_name.encode_utf16().collect();
    let name_offset =
        std::mem::offset_of!(RenamePrefix, file_name_length) + std::mem::size_of::<u32>();
    let total_len = name_offset + wide.len() * std::mem::size_of::<u16>();
    let mut buffer = vec![0u8; total_len.max(std::mem::size_of::<RenamePrefix>())];
    unsafe {
        let prefix = buffer.as_mut_ptr() as *mut RenamePrefix;
        std::ptr::write_unaligned(
            prefix,
            RenamePrefix {
                replace_if_exists: 0,
                root_directory: parent_handle.as_raw_handle() as *mut c_void,
                file_name_length: (wide.len() * 2) as u32,
            },
        );
        std::ptr::copy_nonoverlapping(
            wide.as_ptr() as *const u8,
            buffer.as_mut_ptr().add(name_offset),
            wide.len() * 2,
        );
    }
    let mut status_block = IoStatusBlock {
        status: IoStatusValue { status: 0 },
        information: 0,
    };
    let status = unsafe {
        NtSetInformationFile(
            source.as_raw_handle() as *mut c_void,
            &mut status_block,
            buffer.as_mut_ptr() as *mut c_void,
            total_len as u32,
            FILE_RENAME_INFO_CLASS,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    drop(source);
    drop(chain);
    Ok(())
}

fn normalize_path(path: &Path) -> String {
    normalize(&path.to_string_lossy())
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
        let relative = target[root.len()..]
            .trim_start_matches(['\\', '/'])
            .to_string();
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

/// 用户主动勾选删除（大文件、重复副本、微信文件、粉碎）时的检查。
///
/// 和 `execution_forbidden` 的区别：允许文档、桌面、下载这些用户目录——
/// 用户勾选的大文件和聊天文件本来就在那里；系统目录、Program Files、
/// 密码库、盘符根、关键系统文件、永不清理名单和重解析点仍然拦住。
pub fn explicit_delete_forbidden(path: &str) -> bool {
    if path_syntax_error(path) {
        return true;
    }
    if is_never_clean(path) {
        return true;
    }
    if is_reparse_point(path) || Path::new(path).is_symlink() {
        return true;
    }
    if is_password_store(path) || is_drive_root(path) || is_critical_file(path) {
        return true;
    }
    is_protected_path(path)
}

/// 按经逐级句柄校验的父目录删除普通文件，不使用可被 junction 替换的路径式回退。
pub fn delete_file_checked(path: &str, roots: &[String], untrusted: bool) -> DeleteOutcome {
    if execution_forbidden(path, untrusted) {
        return DeleteOutcome::Skipped;
    }
    match delete_plain_file(Path::new(path), roots) {
        Ok(true) => DeleteOutcome::Deleted,
        Ok(false) => DeleteOutcome::Failed(format!("无法删除：{}", path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => DeleteOutcome::Skipped,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput
            ) =>
        {
            DeleteOutcome::Skipped
        }
        Err(error) => DeleteOutcome::Failed(format!("{}：{}", path, error)),
    }
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

/// 自底向上删除空目录（不删 root 本身），never_clean 命中的目录保留。
pub fn remove_empty_dirs(root: &str) -> usize {
    let mut dirs = Vec::new();
    let mut stack = vec![normalize(root)];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
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
        if remove_directory_checked(Path::new(&path), &[root.to_string()]).unwrap_or(false) {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;

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
        assert!(execution_forbidden(
            "C:\\Windows\\System32\\kernel32.dll",
            true
        ));
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
        let other = vec![std::env::temp_dir()
            .join("cdc-other")
            .to_string_lossy()
            .to_string()];
        assert!(matches!(
            delete_file_checked(&path, &other, true),
            DeleteOutcome::Skipped
        ));
        assert!(Path::new(&path).exists());

        // 在根目录里、位于 Temp 下：允许删除。
        assert!(matches!(
            delete_file_checked(&path, &roots, true),
            DeleteOutcome::Deleted
        ));
        assert!(!Path::new(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_and_directory_operations_refuse_junction_components() {
        let base = std::env::temp_dir().join(format!(
            "cdc-handle-relative-{}-{}",
            std::process::id(),
            crate::timeutil::stamp()
        ));
        let root = base.join("root");
        let outside = base.join("outside");
        let junction = root.join("redirect");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("keep.txt");
        let outside_journal = outside.join("im_move_pending.json");
        std::fs::write(&outside_file, b"must survive").unwrap();
        std::fs::write(&outside_journal, b"must not be read or overwritten").unwrap();
        let command = format!(
            "mklink /J \"{}\" \"{}\"",
            junction.display(),
            outside.display()
        );
        let output = std::process::Command::new(crate::system32("cmd.exe"))
            .arg("/C")
            .raw_arg(command)
            .output()
            .expect("cmd.exe must exist on the Windows test runner");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let roots = [root.to_string_lossy().to_string()];
        assert!(open_read_file_checked(&junction.join("im_move_pending.json"), &roots).is_err());
        assert!(create_new_file_checked(&junction.join("new-journal.json"), &roots).is_err());
        let redirected_target = junction.join("must-not-create");
        assert!(create_directory_tree_checked(&redirected_target).is_err());
        assert!(!outside.join("must-not-create").exists());
        assert!(matches!(
            delete_file_checked(&junction.join("keep.txt").to_string_lossy(), &roots, false),
            DeleteOutcome::Skipped
        ));
        assert!(!remove_directory_checked(&junction, &roots).unwrap());
        assert!(outside_file.exists());
        assert!(junction.exists());

        std::fs::remove_dir(&junction).unwrap();
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn directory_rename_uses_sibling_names_under_a_plain_parent() {
        let base = std::env::temp_dir().join(format!(
            "cdc-handle-rename-{}-{}",
            std::process::id(),
            crate::timeutil::stamp()
        ));
        let source = base.join("source");
        let backup = base.join("source_backup_20261116_200000");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("data.bin"), b"keep data").unwrap();
        let allowed = [source.to_string_lossy().to_string()];

        rename_directory_sibling_checked(&source, &backup, &allowed).unwrap();
        assert!(!source.exists());
        assert!(backup.join("data.bin").exists());
        rename_directory_sibling_checked(&backup, &source, &allowed).unwrap();
        assert!(source.join("data.bin").exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn user_library_and_system_detection() {
        assert!(is_system_path("C:\\Windows\\Temp"));
        assert!(!is_system_path(
            "C:\\Users\\Administrator\\AppData\\Local\\Temp"
        ));
        assert!(is_within(
            "C:\\Users\\Administrator\\AppData\\Local\\Temp\\a\\b.txt",
            &["C:\\Users\\Administrator\\AppData\\Local\\Temp".to_string()]
        ));
    }

    #[test]
    fn explicit_delete_allows_user_folders_but_blocks_system() {
        let profile = std::env::var("USERPROFILE").unwrap_or_default();
        // 用户勾选的大文件/聊天文件通常在下载、文档里，必须允许。
        assert!(!explicit_delete_forbidden(&format!(
            "{}\\Downloads\\movie.mp4",
            profile
        )));
        assert!(!explicit_delete_forbidden(&format!(
            "{}\\Documents\\WeChat Files\\a.jpg",
            profile
        )));
        // 系统目录、关键文件、密码库仍然拒绝。
        assert!(explicit_delete_forbidden(
            "C:\\Windows\\System32\\kernel32.dll"
        ));
        assert!(explicit_delete_forbidden("C:\\pagefile.sys"));
        assert!(explicit_delete_forbidden(
            "C:\\Users\\Administrator\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Login Data"
        ));
        assert!(explicit_delete_forbidden("C:\\Program Files\\App\\a.dll"));
        assert!(explicit_delete_forbidden("C:\\"));
    }
}
