//! 系统级深度瘦身与维护：
//! 1. Windows 更新与组件库清理（SoftwareDistribution、DeliveryOptimization、DISM）
//! 2. 休眠文件管理（hiberfil.sys 检测、精简与关闭）
//! 3. 全局回收站清空（Win32 SHEmptyRecycleBinW）
//! 4. 资源管理器联动（定位文件、打开文件）
//! 5. 微信 / QQ 数据迁移（C 盘文件搬家到其他磁盘 + NTFS 联接）

use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

use crate::safety;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn windows_dir() -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string()))
}

fn programdata_dir() -> PathBuf {
    PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_string()))
}

fn hiber_path() -> PathBuf {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    PathBuf::from(format!("{}\\hiberfil.sys", drive.trim_end_matches('\\')))
}

fn update_download_dir() -> PathBuf {
    windows_dir().join("SoftwareDistribution").join("Download")
}

fn delivery_opt_dir() -> PathBuf {
    windows_dir()
        .join("SoftwareDistribution")
        .join("DeliveryOptimization")
}

fn crash_dump_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        windows_dir().join("Minidump"),
        programdata_dir().join("Microsoft\\Windows\\WER"),
    ];
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("CrashDumps"));
    }
    dirs
}

#[link(name = "shell32")]
extern "system" {
    fn SHEmptyRecycleBinW(hwnd: *mut c_void, psz_root_path: *const u16, dw_flags: u32) -> i32;
}

const SHERB_NOCONFIRMATION: u32 = 0x0000_0001;
const SHERB_NOPROGRESSUI: u32 = 0x0000_0002;
const SHERB_NOSOUND: u32 = 0x0000_0004;

#[derive(Serialize, Deserialize, Clone)]
pub struct SystemItem {
    pub key: String,
    pub name: String,
    pub describe: String,
    pub size: u64,
    pub count: u64,
    pub safe_to_clean: bool,
    pub is_command: bool,
}

#[derive(Serialize)]
pub struct HiberInfo {
    pub enabled: bool,
    pub size: u64,
    pub path: String,
    pub description: String,
}

#[derive(Serialize)]
pub struct MoveAnalysis {
    pub source: String,
    pub exists: bool,
    pub size: u64,
    pub file_count: u64,
    pub is_junction: bool,
    pub target_drives: Vec<DriveTarget>,
}

#[derive(Serialize)]
pub struct DriveTarget {
    pub letter: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

// ---------- 1. 资源管理器联动 ----------

/// explorer.exe 在 Windows 目录下，不在 System32 里，不能复用 system32()。
fn explorer_exe() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{}\\explorer.exe", root.trim_end_matches('\\'))
}

/// 在 Windows 资源管理器中高亮选中指定文件或目录。
pub fn reveal_in_explorer(path: &str) -> Result<(), String> {
    let target = Path::new(path);
    if !target.exists() {
        return Err(format!("文件不存在：{}", path));
    }
    let arg = format!("/select,\"{}\"", path.replace('/', "\\"));
    Command::new(explorer_exe())
        .raw_arg(arg)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("启动资源管理器失败：{}", e))?;
    Ok(())
}

/// 用系统默认程序或资源管理器打开指定路径。
pub fn open_path(path: &str) -> Result<(), String> {
    let target = Path::new(path);
    if !target.exists() {
        return Err(format!("路径不存在：{}", path));
    }
    Command::new(explorer_exe())
        .arg(path.replace('/', "\\"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("打开路径失败：{}", e))?;
    Ok(())
}

// ---------- 2. 休眠文件管理 ----------

pub fn query_hiber() -> HiberInfo {
    let path = hiber_path();
    let mut size = 0u64;
    let mut exists = false;
    if let Ok(meta) = path.metadata() {
        size = meta.len();
        exists = true;
    }
    HiberInfo {
        enabled: exists,
        size,
        path: path.to_string_lossy().to_string(),
        description: if exists {
            format!(
                "休眠文件正在占用 {:.2} GB 空间。若无需使用系统休眠（如只用睡眠），可一键关闭释放空间。",
                size as f64 / (1024.0 * 1024.0 * 1024.0)
            )
        } else {
            "系统休眠未开启，未占用磁盘空间。".to_string()
        },
    }
}

pub fn set_hiber(mode: &str) -> Result<String, String> {
    let args: Vec<&str> = match mode {
        "off" => vec!["/h", "off"],
        "reduced" => vec!["/h", "/type", "reduced"],
        "full" | "on" => vec!["/h", "on"],
        _ => return Err("无效的休眠模式参数".to_string()),
    };
    let output = Command::new(crate::system32("powercfg.exe"))
        .args(&args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("执行 powercfg 失败：{}", e))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("设置休眠失败：{}", err.trim()));
    }
    Ok(match mode {
        "off" => "休眠功能已彻底关闭，hiberfil.sys 已移除并释放全部空间。".to_string(),
        "reduced" => "休眠文件已精简为 Reduced 模式（体积缩减约 50%）。".to_string(),
        _ => "休眠功能已开启。".to_string(),
    })
}

// ---------- 3. 全局回收站清空 ----------

pub fn empty_all_recycle_bins() -> Result<(), String> {
    let code = unsafe {
        SHEmptyRecycleBinW(
            std::ptr::null_mut(),
            std::ptr::null(),
            SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
        )
    };
    // S_OK = 0, S_FALSE = 1
    if code == 0 || code == 1 {
        Ok(())
    } else {
        Err(format!("清空回收站返回错误码：0x{:X}", code))
    }
}

// ---------- 4. 系统更新与深度缓存扫描与清理 ----------

pub fn scan_system_deep() -> Vec<SystemItem> {
    let mut items = Vec::new();

    // 1. Windows Update 下载缓存 (SoftwareDistribution\Download)
    let update_dl = update_download_dir();
    if update_dl.is_dir() {
        let (size, count) = dir_size_count(&update_dl);
        items.push(SystemItem {
            key: "sys_update_dl".to_string(),
            name: "Windows 更新下载安装包".to_string(),
            describe: "系统更新完成后的旧安装包和补丁残留，清理后不影响已安装的更新".to_string(),
            size,
            count,
            safe_to_clean: true,
            is_command: false,
        });
    }

    // 2. 传递优化缓存 (Delivery Optimization)
    let do_cache = delivery_opt_dir();
    if do_cache.is_dir() {
        let (size, count) = dir_size_count(&do_cache);
        items.push(SystemItem {
            key: "sys_delivery_opt".to_string(),
            name: "Windows 传递优化网络缓存".to_string(),
            describe: "微软更新与其他电脑局域网共享的下载分片缓存".to_string(),
            size,
            count,
            safe_to_clean: true,
            is_command: false,
        });
    }

    // 3. 系统崩溃转储与错误报告 (Crash Dumps / WER)
    let mut dump_size = 0u64;
    let mut dump_count = 0u64;
    for p in crash_dump_dirs() {
        if p.is_dir() {
            let (s, c) = dir_size_count(&p);
            dump_size += s;
            dump_count += c;
        }
    }
    items.push(SystemItem {
        key: "sys_crash_dumps".to_string(),
        name: "系统崩溃转储与错误报告".to_string(),
        describe: "软件或系统蓝屏崩溃时记录的 .dmp 文件与诊断日志".to_string(),
        size: dump_size,
        count: dump_count,
        safe_to_clean: true,
        is_command: false,
    });

    // 4. DISM 组件库沉淀（WinSxS 深度清理）
    items.push(SystemItem {
        key: "sys_dism_winsxs".to_string(),
        name: "WinSxS 系统组件库精简 (DISM)".to_string(),
        describe: "执行 DISM 原生命令精简 WinSxS 目录下的历史备份更新，通常可释放 3~15GB（耗时约 1~3 分钟）".to_string(),
        size: 0,
        count: 0,
        safe_to_clean: true,
        is_command: true,
    });

    items
}

pub fn clean_system_item(key: &str) -> Result<(u64, u64, String), String> {
    match key {
        "sys_update_dl" => {
            // 更新服务运行时删 Download 可能打断正在进行的更新：先停 wuauserv/BITS，清完再按原状态启动。
            crate::task::set_stage("system", "停止更新服务");
            let stopped = stop_services(&["wuauserv", "bits"])?;
            crate::task::set_stage("system", "清理更新安装包");
            let dir = update_download_dir();
            let (freed, count) = clean_dir_contents(&dir);
            crate::task::set_stage("system", "恢复更新服务");
            start_services(&stopped);
            Ok((
                freed,
                count,
                format!(
                    "已清理更新安装包 {} 个，释放 {:.2} MB",
                    count,
                    freed as f64 / 1_048_576.0
                ),
            ))
        }
        "sys_delivery_opt" => {
            // 交给系统自带的 cmdlet 清理，它会通过 DoSvc 删除缓存并保留服务自身的状态文件。
            crate::task::set_stage("system", "调用系统清理传递优化缓存");
            let dir = delivery_opt_dir();
            let (before, before_count) = dir_size_count(&dir);
            let output = Command::new(crate::system32("WindowsPowerShell\\v1.0\\powershell.exe"))
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    "Delete-DeliveryOptimizationCache -Force -ErrorAction Stop",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| format!("执行传递优化清理失败：{}", e))?;
            if !output.status.success() {
                let err = String::from_utf8_lossy(&output.stderr);
                return Err(format!("传递优化缓存清理失败：{}", err.trim()));
            }
            let (after, after_count) = dir_size_count(&dir);
            let freed = before.saturating_sub(after);
            let count = before_count.saturating_sub(after_count);
            Ok((
                freed,
                count,
                format!(
                    "已清理传递优化分片 {} 个，释放 {:.2} MB",
                    count,
                    freed as f64 / 1_048_576.0
                ),
            ))
        }
        "sys_crash_dumps" => {
            crate::task::set_stage("system", "清理崩溃转储与错误报告");
            let mut total_freed = 0u64;
            let mut total_count = 0u64;
            for p in crash_dump_dirs() {
                let (f, c) = clean_dir_contents(&p);
                total_freed += f;
                total_count += c;
            }
            Ok((
                total_freed,
                total_count,
                format!(
                    "已清理转储文件 {} 个，释放 {:.2} MB",
                    total_count,
                    total_freed as f64 / 1_048_576.0
                ),
            ))
        }
        "sys_dism_winsxs" => {
            // 执行 DISM 组件清理
            crate::task::set_stage("system", "DISM 组件库精简（约 1~3 分钟）");
            let output = Command::new(crate::system32("Dism.exe"))
                .args(["/Online", "/Cleanup-Image", "/StartComponentCleanup"])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| format!("执行 DISM 失败：{}", e))?;
            if output.status.success() {
                Ok((0, 0, "DISM WinSxS 组件库精简优化已成功完成！".to_string()))
            } else {
                let err = String::from_utf8_lossy(&output.stderr);
                Err(format!("DISM 组件精简失败：{}", err.trim()))
            }
        }
        _ => Err("未知系统清理项".to_string()),
    }
}

fn dir_size_count(dir: &Path) -> (u64, u64) {
    let mut size = 0u64;
    let mut count = 0u64;
    for entry in WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            if let Ok(m) = entry.metadata() {
                size += m.len();
                count += 1;
            }
        }
    }
    (size, count)
}

/// Treat metadata failures as unsafe. Missing leaves are expected while recovering,
/// but all existing ancestors still have to be inspectable and plain directories.
fn path_has_reparse_ancestor(path: &Path) -> bool {
    let mut current = Some(path);
    while let Some(component) = current {
        match safety::reparse_point_status(&component.to_string_lossy()) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return true,
        }
        current = component.parent();
    }
    false
}

/// 清空目录内容（保留目录本身）。不跟随任何重解析点，逐个文件走
/// `safety::delete_file_checked`（句柄删除 + 父目录无联接校验），再自底向上删空目录。
fn clean_dir_contents(dir: &Path) -> (u64, u64) {
    let root = dir.to_string_lossy().to_string();
    if !dir.is_dir() || safety::is_reparse_point(&root) {
        return (0, 0);
    }
    let roots = vec![root.clone()];
    let mut freed = 0u64;
    let mut count = 0u64;
    let mut walker = WalkDir::new(dir)
        .min_depth(1)
        .follow_links(false)
        .into_iter();
    while let Some(entry) = walker.next() {
        if crate::task::is_cancelled("system") {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path().to_string_lossy().to_string();
        if entry.path_is_symlink() || safety::is_reparse_point(&path) {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let len = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        if matches!(
            safety::delete_file_checked(&path, &roots, false),
            safety::DeleteOutcome::Deleted
        ) {
            freed += len;
            count += 1;
            crate::task::add_done("system", 1);
        }
    }
    safety::remove_empty_dirs(&root);
    (freed, count)
}

fn service_running(name: &str) -> bool {
    Command::new(crate::system32("sc.exe"))
        .args(["query", name])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("RUNNING"))
        .unwrap_or(false)
}

/// 停止仍在运行的服务，返回本次停掉的服务名；任何一个停不下来就把已停的恢复并报错。
fn stop_services(names: &[&str]) -> Result<Vec<String>, String> {
    let mut stopped = Vec::new();
    for &name in names {
        if !service_running(name) {
            continue;
        }
        let _ = Command::new(crate::system32("net.exe"))
            .args(["stop", name, "/y"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        if service_running(name) {
            start_services(&stopped);
            return Err(format!(
                "{} 服务正在工作且无法暂停，请等待 Windows 更新完成后再清理",
                name
            ));
        }
        stopped.push(name.to_string());
    }
    Ok(stopped)
}

fn start_services(names: &[String]) {
    for name in names.iter().rev() {
        let _ = Command::new(crate::system32("net.exe"))
            .args(["start", name])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

// ---------- 5. 微信 / QQ C盘文件一键搬家 ----------

/// 分析指定应用在 C 盘的数据位置与可迁移磁盘列表。
pub fn analyze_im_migration(kind: &str) -> MoveAnalysis {
    let roots = crate::im::roots(kind);
    // 优先选择体积最大且位于 C: 的文件夹作为迁移源
    let mut best_source = String::new();
    let mut max_size = 0u64;
    let mut max_count = 0u64;

    for root in &roots {
        let norm = safety::normalize(root);
        // AppData 下是客户端程序数据，搬它没有意义，也不是「聊天文件」。
        if crate::im::is_program_root(&norm) {
            continue;
        }
        if norm.to_uppercase().starts_with("C:") {
            let (sz, cnt) = dir_size_count(Path::new(&norm));
            if sz > max_size {
                max_size = sz;
                max_count = cnt;
                best_source = norm;
            }
        }
    }

    let is_junction = if !best_source.is_empty() {
        safety::is_reparse_point(&best_source)
    } else {
        false
    };

    // 查找非 C: 的固定磁盘
    let mut targets = Vec::new();
    for drive in crate::fixed_drives() {
        if !drive.to_uppercase().starts_with("C:") {
            if let Ok(free) = crate::get_drive_free_space(&drive) {
                targets.push(DriveTarget {
                    letter: drive.clone(),
                    free_bytes: free.0,
                    total_bytes: free.1,
                });
            }
        }
    }

    MoveAnalysis {
        source: best_source.clone(),
        exists: !best_source.is_empty(),
        size: max_size,
        file_count: max_count,
        is_junction,
        target_drives: targets,
    }
}

#[derive(Serialize, Deserialize)]
struct MoveJournal {
    source: String,
    backup: String,
    #[serde(default)]
    kind: String,
}

fn journal_path() -> PathBuf {
    crate::cache_dir().join("im_move_pending.json")
}

fn write_journal(source: &Path, backup: &Path, kind: &str) -> io::Result<()> {
    let journal = MoveJournal {
        source: source.to_string_lossy().to_string(),
        backup: backup.to_string_lossy().to_string(),
        kind: kind.to_string(),
    };
    let text = serde_json::to_vec(&journal)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let path = journal_path();
    let roots = [crate::cache_dir().to_string_lossy().to_string()];
    let result = {
        let (_parent_handles, mut file) = safety::create_new_file_checked(&path, &roots)?;
        file.write_all(&text).and_then(|()| file.sync_all())
    };
    if result.is_err() {
        let _ = safety::delete_file_checked(&path.to_string_lossy(), &roots, false);
    }
    result
}

enum PersistActionError {
    Persist(io::Error),
    Action(io::Error),
}

fn persist_before_action<T>(
    persist: impl FnOnce() -> io::Result<()>,
    action: impl FnOnce() -> io::Result<T>,
) -> Result<T, PersistActionError> {
    persist().map_err(PersistActionError::Persist)?;
    action().map_err(PersistActionError::Action)
}

fn clear_journal_after_recovery(result: io::Result<()>, clear: impl FnOnce()) -> bool {
    if result.is_ok() {
        clear();
        true
    } else {
        false
    }
}

fn clear_journal_if_backup_absent(backup: &Path, clear: impl FnOnce()) -> bool {
    if matches!(
        std::fs::symlink_metadata(backup),
        Err(ref error) if error.kind() == io::ErrorKind::NotFound
    ) {
        clear();
        true
    } else {
        false
    }
}

fn clear_journal() {
    let path = journal_path();
    let roots = [crate::cache_dir().to_string_lossy().to_string()];
    let _ = safety::delete_file_checked(&path.to_string_lossy(), &roots, false);
}

fn read_journal() -> io::Result<String> {
    let path = journal_path();
    let roots = [crate::cache_dir().to_string_lossy().to_string()];
    let (_parent_handles, mut file) = safety::open_read_file_checked(&path, &roots)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// 启动时调用：上次搬家在「原目录改名」之后、「建立联接」之前中断时，
/// 原路径是空的、数据还在备份目录里，这里把它改回原名。
pub fn recover_pending_move() {
    let text = match read_journal() {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(_) => return,
    };
    let Ok(journal) = serde_json::from_str::<MoveJournal>(&text) else {
        // Preserve malformed records for diagnosis; an incomplete write must not
        // silently erase the only indication that recovery was expected.
        return;
    };
    let allowed_roots = match journal.kind.as_str() {
        "wechat" => crate::im::migration_roots("wechat"),
        "qq" => crate::im::migration_roots("qq"),
        // 兼容旧版日志：仅允许源路径精确匹配微信或 QQ 的已配置根目录。
        "" => crate::im::migration_roots("wechat")
            .into_iter()
            .chain(crate::im::migration_roots("qq"))
            .collect(),
        _ => return,
    };
    if !valid_recovery_paths(&journal.source, &journal.backup, &allowed_roots)
        || path_has_reparse_parent(Path::new(&journal.source))
        || path_has_reparse_parent(Path::new(&journal.backup))
    {
        return;
    }
    let source = Path::new(&journal.source);
    let backup = Path::new(&journal.backup);
    match std::fs::symlink_metadata(source) {
        // A source entry means never overwrite it with the backup.  A leftover
        // backup keeps the journal for diagnosis/retry instead of losing state.
        Ok(_) => {
            if !clear_journal_if_backup_absent(backup, clear_journal) {
                crate::logs::write_log(&[format!(
                    "搬家恢复日志保留：源路径已存在，但备份目录仍在或状态不确定：{}",
                    journal.backup
                )]);
            }
            return;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return,
    }
    let backup_metadata = match std::fs::symlink_metadata(backup) {
        Ok(metadata) => metadata,
        Err(_) => return,
    };
    if !backup_metadata.is_dir() || backup_metadata.file_attributes() & 0x400 != 0 {
        return;
    }
    let restore = safety::rename_directory_sibling_checked(backup, source, &allowed_roots);
    if restore.is_ok() {
        crate::logs::write_log(&[format!(
            "微信/QQ 搬家中断恢复：{} 已还原为 {}",
            journal.backup, journal.source
        )]);
    }
    let _ = clear_journal_after_recovery(restore, clear_journal);
}

fn path_has_reparse_parent(path: &Path) -> bool {
    path.parent().map(path_has_reparse_ancestor).unwrap_or(true)
}

fn valid_recovery_paths(source: &str, backup: &str, allowed_roots: &[String]) -> bool {
    if safety::path_syntax_error(source)
        || safety::path_syntax_error(backup)
        || safety::is_protected_path(source)
        || safety::is_never_clean(source)
        || safety::is_drive_root(source)
        || safety::is_critical_file(source)
        || safety::is_password_store(source)
    {
        return false;
    }
    let source_path = Path::new(source);
    let backup_path = Path::new(backup);
    if !source_path.is_absolute() || !backup_path.is_absolute() {
        return false;
    }

    let source_normalized = safety::normalize(source);
    if !allowed_roots
        .iter()
        .any(|root| safety::normalize(root).eq_ignore_ascii_case(&source_normalized))
    {
        return false;
    }

    let (Some(source_parent), Some(backup_parent), Some(source_name), Some(backup_name)) = (
        source_path.parent(),
        backup_path.parent(),
        source_path.file_name(),
        backup_path.file_name(),
    ) else {
        return false;
    };
    if !safety::normalize(&source_parent.to_string_lossy())
        .eq_ignore_ascii_case(&safety::normalize(&backup_parent.to_string_lossy()))
    {
        return false;
    }

    let backup_name = backup_name.to_string_lossy().to_lowercase();
    let prefix = format!("{}_backup_", source_name.to_string_lossy().to_lowercase());
    let Some(stamp) = backup_name.strip_prefix(&prefix) else {
        return false;
    };
    stamp.len() == 15
        && stamp.as_bytes()[8] == b'_'
        && stamp
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 8 || byte.is_ascii_digit())
}

fn safe_for_quoted_cmd_path(path: &str) -> bool {
    !path.is_empty()
        && !path
            .chars()
            .any(|character| character == '"' || character == '%' || character.is_control())
}

/// 执行数据迁移：将 source 目录完整搬到 target_dir，并在原位置建立 NTFS Junction 联接。
pub fn execute_im_migration(
    source_str: &str,
    target_dir_str: &str,
    kind: &str,
) -> Result<String, String> {
    let source = Path::new(source_str);
    let target = Path::new(target_dir_str);

    if !safe_for_quoted_cmd_path(source_str) || !safe_for_quoted_cmd_path(target_dir_str) {
        return Err("迁移路径包含不受支持的命令行字符".to_string());
    }
    if !source.exists() || !source.is_dir() {
        return Err("源数据目录不存在".to_string());
    }
    if kind != "wechat" && kind != "qq" {
        return Err("无效的搬家类型".to_string());
    }

    // 迁移和恢复日志都只接受探测到的根目录本身，不接受其下任意子目录。
    let roots = crate::im::roots(kind);
    let source_norm = safety::normalize(source_str);
    if !roots
        .iter()
        .any(|root| safety::normalize(root).eq_ignore_ascii_case(&source_norm))
    {
        return Err("只能迁移微信/QQ 自己探测到的数据根目录，已拒绝本次操作".to_string());
    }
    if safety::explicit_delete_forbidden(source_str) {
        return Err("该目录受保护，不能迁移".to_string());
    }
    match safety::reparse_point_status(source_str) {
        Ok(true) => return Err("源目录已经是软链接/联接点，之前已经完成过搬家".to_string()),
        Ok(false) => {}
        Err(error) => return Err(format!("无法安全检查源目录：{}", error)),
    }

    if safety::is_within(target_dir_str, &[source_str.to_string()]) {
        return Err("目标目录不能位于源目录内部".to_string());
    }
    if target.exists() {
        return Err("目标目录已存在，请换一个位置或先手动清理".to_string());
    }

    // 检查源目录是否正在被微信/QQ占用
    let active_procs = crate::process::check_running(&["wechat.exe", "qq.exe", "xwechat.exe"]);
    if !active_procs.is_empty() {
        return Err(format!(
            "检测到 {} 正在运行，请先彻底退出该软件后再执行搬家！",
            active_procs.join("、")
        ));
    }

    // 计算源大小与目标磁盘剩余空间
    crate::task::set_stage("im_move", "统计源数据");
    let (src_size, file_cnt) = dir_size_count(source);
    if src_size == 0 || file_cnt == 0 {
        return Err("源数据目录是空的，无需搬家".to_string());
    }
    let target_norm = safety::normalize(target_dir_str);
    let target_drive = target_norm.chars().take(3).collect::<String>();
    if !crate::fixed_drives()
        .iter()
        .any(|drive| drive.eq_ignore_ascii_case(&target_drive))
    {
        return Err("目标位置不是本机固定磁盘".to_string());
    }
    if let Ok((free, _)) = crate::get_drive_free_space(&target_drive) {
        if free < src_size + (1024 * 1024 * 1024) {
            return Err("目标磁盘剩余空间不足，请选择剩余空间更充裕的磁盘".to_string());
        }
    }

    // 句柄相对创建目标并复制；保留源/目标目录链，阻止路径在校验期间被替换。
    crate::task::set_stage("im_move", "复制文件");
    crate::task::set_unit("im_move", "字节");
    crate::task::set_progress("im_move", 0, src_size);
    let (source_handles, target_handles) = copy_dir_recursive(source, target)
        .map_err(|error| format!("拷贝文件至新目录失败：{}", error))?;
    crate::task::set_stage("im_move", "校验文件");
    crate::task::set_unit("im_move", "个文件");
    crate::task::set_progress("im_move", 0, file_cnt);
    let (dst_size, dst_cnt) = dir_size_count(target);
    if dst_size != src_size || dst_cnt != file_cnt {
        drop(source_handles);
        let _ = cleanup_target(target, target_handles);
        return Err(format!(
            "拷贝校验失败（源 {} 个文件 / {} 字节，目标 {} 个文件 / {} 字节），已放弃搬家并清理目标目录",
            file_cnt, src_size, dst_cnt, dst_size
        ));
    }
    if let Err(detail) = verify_copy(source, target) {
        drop(source_handles);
        let _ = cleanup_target(target, target_handles);
        return Err(format!(
            "逐文件校验失败（{}），已放弃搬家并清理目标目录",
            detail
        ));
    }
    drop(source_handles);

    // 3. 将原目录重命名为临时备份
    crate::task::set_stage("im_move", "备份原目录");
    let backup_path = source.with_file_name(format!(
        "{}_backup_{}",
        source.file_name().unwrap_or_default().to_string_lossy(),
        crate::timeutil::stamp()
    ));
    match persist_before_action(
        || write_journal(source, &backup_path, kind),
        || safety::rename_directory_sibling_checked(source, &backup_path, &roots),
    ) {
        Ok(()) => {}
        Err(PersistActionError::Persist(error)) => {
            let _ = cleanup_target(target, target_handles);
            return Err(if error.kind() == io::ErrorKind::AlreadyExists {
                "发现尚未解决的搬家恢复日志，原目录未改名；请先处理上次搬家留下的备份状态。"
                    .to_string()
            } else {
                format!("搬家恢复日志未能可靠写入，原目录未改名：{}", error)
            });
        }
        Err(PersistActionError::Action(error)) => {
            clear_journal();
            let _ = cleanup_target(target, target_handles);
            return Err(format!("备份原目录失败（可能被占用）：{}", error));
        }
    }

    // 4. 在原位置创建 NTFS Junction (mklink /J)
    crate::task::set_stage("im_move", "创建目录联接");
    let output = Command::new(crate::system32("cmd.exe"))
        .arg("/D")
        .arg("/C")
        .raw_arg(format!(
            "mklink /J \"{}\" \"{}\"",
            source_str.replace('/', "\\").trim_end_matches('\\'),
            target_dir_str.replace('/', "\\").trim_end_matches('\\')
        ))
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            // Junction 创建成功！删除原目录备份，释放 C 盘空间
            crate::task::set_stage("im_move", "清理备份");
            let backup_root = [backup_path.to_string_lossy().to_string()];
            let removed = safety::remove_tree_checked(&backup_path, &backup_root).is_ok();
            if removed {
                clear_journal();
            } else {
                crate::logs::write_log(&[format!(
                    "搬家已完成，但备份清理未完成；恢复日志保留：{}",
                    backup_path.display()
                )]);
            }
            Ok(format!(
                "数据搬家成功！已将 {} 个文件（{:.2} GB）迁移至 {}，原 C 盘空间已释放{}。",
                file_cnt,
                src_size as f64 / (1024.0 * 1024.0 * 1024.0),
                target_dir_str,
                if removed {
                    ""
                } else {
                    "（备份目录删除失败，请手动清理残留）"
                }
            ))
        }
        _ => {
            // Junction 失败时只在句柄相对恢复成功后清日志；失败则留待下次启动重试。
            if let Err(error) =
                safety::rename_directory_sibling_checked(&backup_path, source, &roots)
            {
                return Err(format!(
                    "创建目录联接失败，自动恢复暂未成功；原数据保留在 {}，恢复日志已保留供下次重试：{}",
                    backup_path.display(), error
                ));
            }
            let target_removed = cleanup_target(target, target_handles);
            clear_journal();
            Err(if target_removed {
                "创建 NTFS 目录联接失败，原数据已自动恢复。".to_string()
            } else {
                "创建 NTFS 目录联接失败，原数据已恢复；目标目录有残留，请手动清理。".to_string()
            })
        }
    }
}

/// Compare each ordinary source file with its handle-opened destination counterpart.
fn verify_copy(src: &Path, dst: &Path) -> Result<(), String> {
    let source_root = [src.to_string_lossy().to_string()];
    let target_root = [dst.to_string_lossy().to_string()];
    let _source_chain =
        safety::pin_directory_checked(src, &source_root).map_err(|e| e.to_string())?;
    let _target_chain =
        safety::pin_directory_checked(dst, &target_root).map_err(|e| e.to_string())?;
    let mut walker = WalkDir::new(src)
        .min_depth(1)
        .follow_links(false)
        .into_iter();
    while let Some(entry) = walker.next() {
        if crate::task::is_cancelled("im_move") {
            return Err("已取消".to_string());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let reparse =
            safety::reparse_point_status(&path.to_string_lossy()).map_err(|e| e.to_string())?;
        if reparse || entry.path_is_symlink() {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = path.strip_prefix(src).map_err(|e| e.to_string())?;
        let copied = dst.join(relative);
        let (_source_parents, mut source_file) = safety::open_read_file_checked(path, &source_root)
            .map_err(|e| format!("{} 无法安全读取：{}", relative.display(), e))?;
        let (_target_parents, mut target_file) =
            safety::open_read_file_checked(&copied, &target_root)
                .map_err(|e| format!("{} 未完整拷贝：{}", relative.display(), e))?;
        if source_file.metadata().map_err(|e| e.to_string())?.len()
            != target_file.metadata().map_err(|e| e.to_string())?.len()
        {
            return Err(format!("{} 大小不一致", relative.display()));
        }
        let mut left = [0u8; 64 * 1024];
        let mut right = [0u8; 64 * 1024];
        loop {
            let left_len = source_file.read(&mut left).map_err(|e| e.to_string())?;
            let right_len = target_file.read(&mut right).map_err(|e| e.to_string())?;
            if left_len != right_len || left[..left_len] != right[..right_len] {
                return Err(format!("{} 内容不一致", relative.display()));
            }
            if left_len == 0 {
                break;
            }
        }
        crate::task::add_done("im_move", 1);
    }
    Ok(())
}

fn copy_dir_contents(
    src: &Path,
    dst: &Path,
    source_root: &[String],
    target_root: &[String],
) -> io::Result<()> {
    let _source_chain = safety::pin_directory_checked(src, source_root)?;
    for entry in std::fs::read_dir(src)? {
        if crate::task::is_cancelled("im_move") {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "已取消"));
        }
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if safety::reparse_point_status(&from.to_string_lossy())? {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let (_parent_chain, _created_dir) = safety::create_directory_checked(&to, target_root)?;
            copy_dir_contents(&from, &to, source_root, target_root)?;
        } else if file_type.is_file() {
            let (_source_parents, mut source_file) =
                safety::open_read_file_checked(&from, source_root)?;
            let (_target_parents, mut target_file) =
                safety::create_new_file_checked(&to, target_root)?;
            let copied = io::copy(&mut source_file, &mut target_file)?;
            crate::task::add_done("im_move", copied);
            target_file.sync_all()?;
        }
    }
    Ok(())
}

fn cleanup_target(target: &Path, handles: Vec<std::fs::File>) -> bool {
    let roots = [target.to_string_lossy().to_string()];
    let first = safety::remove_tree_checked(target, &roots).is_ok();
    drop(handles);
    first || safety::remove_tree_checked(target, &roots).is_ok()
}

/// Copy a directory using handle-relative file opens and atomic create-only destination entries.
/// Returned source/target chains pin the roots until verification and source rename are complete.
fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
) -> io::Result<(Vec<std::fs::File>, Vec<std::fs::File>)> {
    let source_root = [src.to_string_lossy().to_string()];
    let target_root = [dst.to_string_lossy().to_string()];
    let source_handles = safety::pin_directory_checked(src, &source_root)?;
    let target_handles = safety::create_directory_tree_checked(dst)?;
    if let Err(error) = copy_dir_contents(src, dst, &source_root, &target_root) {
        let _ = safety::remove_tree_checked(dst, &target_root);
        drop(source_handles);
        let _ = cleanup_target(dst, target_handles);
        return Err(error);
    }
    Ok((source_handles, target_handles))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn query_hiber_returns_valid_struct() {
        let hiber = query_hiber();
        assert!(hiber.path.ends_with("hiberfil.sys"));
    }

    #[test]
    fn scan_system_deep_returns_items() {
        let items = scan_system_deep();
        assert!(!items.is_empty());
        assert!(items.iter().any(|i| i.key == "sys_dism_winsxs"));
    }

    #[test]
    fn migration_rejects_non_im_source() {
        let result = execute_im_migration("C:\\Windows", "D:\\cdc_should_not_exist", "wechat");
        assert!(result.is_err());
        assert!(!Path::new("D:\\cdc_should_not_exist").exists());
    }

    #[test]
    fn copy_dir_recursive_matches_source_size() {
        let base = std::env::temp_dir().join(format!(
            "cdc-copy-{}-{}",
            std::process::id(),
            crate::timeutil::stamp()
        ));
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub").join("b.txt"), b"world!").unwrap();
        let (source_handles, target_handles) = copy_dir_recursive(&src, &dst).unwrap();
        assert_eq!(dir_size_count(&src), dir_size_count(&dst));
        assert!(verify_copy(&src, &dst).is_ok());
        std::fs::write(dst.join("sub").join("b.txt"), b"w").unwrap();
        assert!(verify_copy(&src, &dst).is_err());
        drop(source_handles);
        drop(target_handles);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn clean_dir_contents_empties_dir_but_keeps_root() {
        let base = std::env::temp_dir().join(format!("cdc-clean-{}", std::process::id()));
        std::fs::create_dir_all(base.join("nested")).unwrap();
        std::fs::write(base.join("a.bin"), b"12345").unwrap();
        std::fs::write(base.join("nested").join("b.bin"), b"678").unwrap();
        let (freed, count) = clean_dir_contents(&base);
        assert_eq!((freed, count), (8, 2));
        assert!(base.is_dir());
        assert!(!base.join("nested").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn recovery_paths_require_an_exact_allowed_source_and_timestamped_sibling_backup() {
        let source = "C:\\Users\\u\\Documents\\WeChat Files";
        let roots = vec![source.to_string()];
        assert!(valid_recovery_paths(
            source,
            "C:\\Users\\u\\Documents\\WeChat Files_backup_20261116_200000",
            &roots
        ));
        assert!(!valid_recovery_paths(
            "C:\\Users\\u\\Documents\\WeChat Files\\nested",
            "C:\\Users\\u\\Documents\\WeChat Files\\nested_backup_20261116_200000",
            &roots
        ));
        assert!(!valid_recovery_paths(
            source,
            "D:\\elsewhere\\WeChat Files_backup_20261116_200000",
            &roots
        ));
        assert!(!valid_recovery_paths(
            source,
            "C:\\Users\\u\\Documents\\WeChat Files_backup_20261116-200000",
            &roots
        ));
        assert!(!valid_recovery_paths(
            "C:\\Windows\\System32",
            "C:\\Windows\\System32_backup_20261116_200000",
            &roots
        ));
    }

    #[test]
    fn cmd_quoted_migration_paths_reject_expansion_and_quote_characters() {
        assert!(safe_for_quoted_cmd_path("D:\\Data folder\\WeChat"));
        assert!(safe_for_quoted_cmd_path("D:\\Data & files\\WeChat"));
        assert!(!safe_for_quoted_cmd_path("D:\\Data%TEMP%\\WeChat"));
        assert!(!safe_for_quoted_cmd_path("D:\\Data\\\"quoted\""));
        assert!(!safe_for_quoted_cmd_path("D:\\Data\\line\nnext"));
    }

    #[test]
    fn journal_write_failure_prevents_source_rename() {
        let rename_called = Cell::new(false);
        let result = persist_before_action(
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "journal unavailable",
                ))
            },
            || {
                rename_called.set(true);
                Ok(())
            },
        );
        assert!(matches!(result, Err(PersistActionError::Persist(_))));
        assert!(!rename_called.get());
    }

    #[test]
    fn failed_recovery_keeps_journal_for_retry() {
        let journal_exists = Cell::new(true);
        assert!(!clear_journal_after_recovery(
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "parent busy"
            )),
            || journal_exists.set(false)
        ));
        assert!(journal_exists.get());
        assert!(clear_journal_after_recovery(Ok(()), || journal_exists.set(false)));
        assert!(!journal_exists.get());
    }

    #[test]
    fn backup_presence_prevents_clearing_journal_while_source_exists() {
        let base = std::env::temp_dir().join(format!(
            "cdc-journal-backup-{}-{}",
            std::process::id(),
            crate::timeutil::stamp()
        ));
        let backup = base.join("backup");
        std::fs::create_dir_all(&backup).unwrap();
        let journal_exists = Cell::new(true);
        assert!(!clear_journal_if_backup_absent(&backup, || journal_exists.set(false)));
        assert!(journal_exists.get());
        std::fs::remove_dir(&backup).unwrap();
        assert!(clear_journal_if_backup_absent(&backup, || journal_exists.set(false)));
        assert!(!journal_exists.get());
        std::fs::remove_dir(base).unwrap();
    }

    #[test]
    fn old_journal_without_kind_remains_deserializable() {
        let journal: MoveJournal = serde_json::from_str(
            r#"{"source":"C:\\Users\\u\\Documents\\WeChat Files","backup":"C:\\Users\\u\\Documents\\WeChat Files_backup_20261116_200000"}"#,
        )
        .unwrap();
        assert!(journal.kind.is_empty());
    }
}
