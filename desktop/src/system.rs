//! 系统级深度瘦身与维护：
//! 1. Windows 更新与组件库清理（SoftwareDistribution、DeliveryOptimization、DISM）
//! 2. 休眠文件管理（hiberfil.sys 检测、精简与关闭）
//! 3. 全局回收站清空（Win32 SHEmptyRecycleBinW）
//! 4. 资源管理器联动（定位文件、打开文件）
//! 5. 微信 / QQ 数据迁移（C 盘文件搬家到其他磁盘 + NTFS 联接）

use serde::{Deserialize, Serialize};
use std::ffi::c_void;
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

/// 在 Windows 资源管理器中高亮选中指定文件或目录。
pub fn reveal_in_explorer(path: &str) -> Result<(), String> {
    let target = Path::new(path);
    if !target.exists() {
        return Err(format!("文件不存在：{}", path));
    }
    let arg = format!("/select,\"{}\"", path.replace('/', "\\"));
    Command::new(crate::system32("explorer.exe"))
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
    Command::new(crate::system32("explorer.exe"))
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
            let dir = update_download_dir();
            let (freed, count) = clean_dir_contents(&dir);
            Ok((freed, count, format!("已清理更新安装包 {} 个，释放 {:.2} MB", count, freed as f64 / 1_048_576.0)))
        }
        "sys_delivery_opt" => {
            let dir = delivery_opt_dir();
            let (freed, count) = clean_dir_contents(&dir);
            Ok((freed, count, format!("已清理传递优化分片 {} 个，释放 {:.2} MB", count, freed as f64 / 1_048_576.0)))
        }
        "sys_crash_dumps" => {
            let mut total_freed = 0u64;
            let mut total_count = 0u64;
            for p in crash_dump_dirs() {
                let (f, c) = clean_dir_contents(&p);
                total_freed += f;
                total_count += c;
            }
            Ok((total_freed, total_count, format!("已清理转储文件 {} 个，释放 {:.2} MB", total_count, total_freed as f64 / 1_048_576.0)))
        }
        "sys_dism_winsxs" => {
            // 执行 DISM 组件清理
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
    for entry in WalkDir::new(dir).min_depth(1).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            if let Ok(m) = entry.metadata() {
                size += m.len();
                count += 1;
            }
        }
    }
    (size, count)
}

fn clean_dir_contents(dir: &Path) -> (u64, u64) {
    if !dir.is_dir() {
        return (0, 0);
    }
    let mut freed = 0u64;
    let mut count = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Ok(m) = p.metadata() {
                    let len = m.len();
                    if std::fs::remove_file(&p).is_ok() {
                        freed += len;
                        count += 1;
                    }
                }
            } else if p.is_dir() {
                let (sub_freed, sub_count) = dir_size_count(&p);
                if std::fs::remove_dir_all(&p).is_ok() {
                    freed += sub_freed;
                    count += sub_count;
                }
            }
        }
    }
    (freed, count)
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
}

fn journal_path() -> PathBuf {
    crate::cache_dir().join("im_move_pending.json")
}

fn write_journal(source: &Path, backup: &Path) {
    let journal = MoveJournal {
        source: source.to_string_lossy().to_string(),
        backup: backup.to_string_lossy().to_string(),
    };
    if let Ok(text) = serde_json::to_string(&journal) {
        let _ = std::fs::write(journal_path(), text);
    }
}

fn clear_journal() {
    let _ = std::fs::remove_file(journal_path());
}

/// 启动时调用：上次搬家在「原目录改名」之后、「建立联接」之前中断时，
/// 原路径是空的、数据还在备份目录里，这里把它改回原名。
pub fn recover_pending_move() {
    let Ok(text) = std::fs::read_to_string(journal_path()) else {
        return;
    };
    let Ok(journal) = serde_json::from_str::<MoveJournal>(&text) else {
        clear_journal();
        return;
    };
    let source = Path::new(&journal.source);
    let backup = Path::new(&journal.backup);
    if backup.is_dir() && !source.exists() {
        if std::fs::rename(backup, source).is_ok() {
            crate::logs::write_log(&[format!(
                "微信/QQ 搬家中断恢复：{} 已还原为 {}",
                journal.backup, journal.source
            )]);
        }
    }
    clear_journal();
}

/// 执行数据迁移：将 source 目录完整搬到 target_dir，并在原位置建立 NTFS Junction 联接。
pub fn execute_im_migration(
    source_str: &str,
    target_dir_str: &str,
    kind: &str,
) -> Result<String, String> {
    let source = Path::new(source_str);
    let target = Path::new(target_dir_str);

    if !source.exists() || !source.is_dir() {
        return Err("源数据目录不存在".to_string());
    }
    if kind != "wechat" && kind != "qq" {
        return Err("无效的搬家类型".to_string());
    }

    // 只允许迁移微信/QQ 自己探测出来的数据目录，避免传错路径把别的目录搬走。
    let roots = crate::im::roots(kind);
    if !safety::is_within(source_str, &roots) {
        return Err("只能迁移微信/QQ 自己的数据目录，已拒绝本次操作".to_string());
    }
    if safety::explicit_delete_forbidden(source_str) {
        return Err("该目录受保护，不能迁移".to_string());
    }
    if safety::is_reparse_point(source_str) {
        return Err("源目录已经是软链接/联接点，之前已经完成过搬家".to_string());
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

    // 确保目标父目录存在
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // 1. 递归拷贝文件与目录
    copy_dir_recursive(source, target)
        .map_err(|e| format!("拷贝文件至新目录失败：{}", e))?;

    // 2. 校验拷贝完整性，确认一致后才动原目录
    let (dst_size, dst_cnt) = dir_size_count(target);
    if dst_size != src_size || dst_cnt != file_cnt {
        let _ = std::fs::remove_dir_all(target);
        return Err(format!(
            "拷贝校验失败（源 {} 个文件 / {} 字节，目标 {} 个文件 / {} 字节），已放弃搬家并清理目标目录",
            file_cnt, src_size, dst_cnt, dst_size
        ));
    }

    // 3. 将原目录重命名为临时备份
    let backup_path = source.with_file_name(format!(
        "{}_backup_{}",
        source.file_name().unwrap_or_default().to_string_lossy(),
        crate::timeutil::stamp()
    ));
    write_journal(source, &backup_path);

    if let Err(e) = std::fs::rename(source, &backup_path) {
        clear_journal();
        return Err(format!("备份原目录失败（可能被占用）：{}", e));
    }

    // 4. 在原位置创建 NTFS Junction (mklink /J)
    let output = Command::new(crate::system32("cmd.exe"))
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
            let removed = std::fs::remove_dir_all(&backup_path).is_ok();
            clear_journal();
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
            // Junction 失败，恢复重命名
            let _ = std::fs::rename(&backup_path, source);
            let _ = std::fs::remove_dir_all(target);
            clear_journal();
            Err("创建 NTFS 目录联接失败，已自动恢复原状。".to_string())
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        // 跳过联接/符号链接：既不复制外部内容，也避免递归死循环。
        if crate::safety::is_reparse_point(&from.to_string_lossy()) {
            continue;
        }
        let file_type = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

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
        let base = std::env::temp_dir().join(format!("cdc-copy-{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub").join("b.txt"), b"world!").unwrap();
        copy_dir_recursive(&src, &dst).unwrap();
        assert_eq!(dir_size_count(&src), dir_size_count(&dst));
        let _ = std::fs::remove_dir_all(&base);
    }
}
