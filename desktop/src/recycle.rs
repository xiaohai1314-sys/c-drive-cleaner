//! 把文件送进回收站（FO_DELETE + FOF_ALLOWUNDO）。
//! 失败时不退回到永久删除，调用方按失败处理。

use std::ffi::c_void;

use crate::safety;

const FO_DELETE: u32 = 3;
const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_NOCONFIRMATION: u16 = 0x0010;
const FOF_NOERRORUI: u16 = 0x0400;

#[repr(C)]
struct ShFileOpStructW {
    hwnd: *mut c_void,
    w_func: u32,
    p_from: *const u16,
    p_to: *const u16,
    f_flags: u16,
    f_any_operations_aborted: i32,
    h_name_mappings: *mut c_void,
    lpsz_progress_title: *const u16,
}

#[link(name = "shell32")]
extern "system" {
    fn SHFileOperationW(operation: *mut ShFileOpStructW) -> i32;
}

/// 返回没能送进回收站的路径。每批 40 个，和 Python 版一致。
pub fn send_to_recycle(paths: &[String]) -> Vec<String> {
    let mut failed = Vec::new();
    let mut eligible = Vec::new();
    for path in paths {
        let is_file = std::fs::metadata(path)
            .map(|meta| meta.is_file())
            .unwrap_or(false);
        if !is_file || safety::explicit_delete_forbidden(path) {
            failed.push(path.clone());
        } else {
            eligible.push(path.clone());
        }
    }
    for chunk in eligible.chunks(40) {
        failed.extend(send_chunk(chunk));
    }
    failed
}

fn send_chunk(paths: &[String]) -> Vec<String> {
    let mut buffer: Vec<u16> = Vec::new();
    for path in paths {
        buffer.extend(path.encode_utf16());
        buffer.push(0);
    }
    buffer.push(0); // 双空字符结尾
    let mut operation = ShFileOpStructW {
        hwnd: std::ptr::null_mut(),
        w_func: FO_DELETE,
        p_from: buffer.as_ptr(),
        p_to: std::ptr::null(),
        f_flags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI,
        f_any_operations_aborted: 0,
        h_name_mappings: std::ptr::null_mut(),
        lpsz_progress_title: std::ptr::null(),
    };
    let code = unsafe { SHFileOperationW(&mut operation) };
    if code == 0 && operation.f_any_operations_aborted == 0 {
        Vec::new()
    } else {
        paths.to_vec()
    }
}
