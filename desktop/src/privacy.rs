//! 隐私痕迹：浏览器历史 / Cookie / 下载记录、最近文件、注册表使用痕迹。
//! 对应 Python 版 privacy.py 的可清理子集：只统计有把握的项目，
//! 密码库和文档库一律不碰；注册表删除前先用 reg.exe 导出备份。

use rusqlite::Connection;
use serde::Serialize;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::logs;
use crate::recycle;
use crate::safety;
use crate::timeutil;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const HKEY_CURRENT_USER: usize = 0x8000_0001;
const HKEY_LOCAL_MACHINE: usize = 0x8000_0002;
const KEY_READ: u32 = 0x0002_0019;
const KEY_ALL_ACCESS: u32 = 0x000F_003F;

const SHOP_DOMAINS: [&str; 10] = [
    "taobao.com",
    "tmall.com",
    "jd.com",
    "yangkeduo.com",
    "pinduoduo.com",
    "amazon.com",
    "amazon.cn",
    "smzdm.com",
    "suning.com",
    "dangdang.com",
];
const FINANCE_DOMAINS: [&str; 7] = [
    "alipay.com",
    "paypal.com",
    "icbc.com.cn",
    "ccb.com",
    "cmbchina.com",
    "unionpay.com",
    "abchina.com",
];
const MEDICAL_DOMAINS: [&str; 5] = [
    "haodf.com",
    "dxy.cn",
    "xywy.com",
    "chunyuyisheng.com",
    "guahao.com",
];
const SENSITIVE_WORDS: [&str; 6] = ["身份证", "密码", "银行卡", "合同", "护照", "工资"];
const RECENT_MEDIA_EXT: [&str; 7] = [".mp4", ".mkv", ".avi", ".mp3", ".flac", ".wav", ".mov"];

#[derive(Serialize, Clone)]
pub struct PrivacyItem {
    pub id: String,
    pub section: String,
    pub name: String,
    pub count: i64,
    pub count_available: bool,
    pub files: Vec<String>,
    pub action: String,
    pub note: String,
}

#[derive(Serialize)]
pub struct PrivacyReport {
    pub files: i64,
    pub sql: i64,
    pub registry: i64,
    pub failed: Vec<String>,
}

#[repr(C)]
struct FileTimeRaw {
    low: u32,
    high: u32,
}

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(
        key: usize,
        subkey: *const u16,
        options: u32,
        access: u32,
        result: *mut usize,
    ) -> i32;
    fn RegEnumKeyExW(
        key: usize,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        class: *mut u16,
        class_len: *mut u32,
        last_write: *mut FileTimeRaw,
    ) -> i32;
    fn RegEnumValueW(
        key: usize,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> i32;
    fn RegDeleteValueW(key: usize, name: *const u16) -> i32;
    fn RegCloseKey(key: usize) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn env_path(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

// ---------- 浏览器 ----------

struct BrowserSpec {
    id: &'static str,
    name: &'static str,
    root: String,
    firefox: bool,
}

fn browser_specs() -> Vec<BrowserSpec> {
    let local = env_path("LOCALAPPDATA");
    let roaming = env_path("APPDATA");
    let mut specs = Vec::new();
    for (id, name, suffix) in [
        ("chrome", "Chrome", "\\Google\\Chrome\\User Data"),
        ("edge", "Edge", "\\Microsoft\\Edge\\User Data"),
        ("qqbrowser", "QQ浏览器", "\\Tencent\\QQBrowser\\User Data"),
        ("quark", "夸克浏览器", "\\Quark\\User Data"),
        (
            "brave",
            "Brave",
            "\\BraveSoftware\\Brave-Browser\\User Data",
        ),
    ] {
        let root = format!("{}{}", local, suffix);
        if Path::new(&root).is_dir() {
            specs.push(BrowserSpec {
                id,
                name,
                root,
                firefox: false,
            });
        }
    }
    let firefox_root = format!("{}\\Mozilla\\Firefox\\Profiles", roaming);
    if Path::new(&firefox_root).is_dir() {
        specs.push(BrowserSpec {
            id: "firefox",
            name: "Firefox",
            root: firefox_root,
            firefox: true,
        });
    }
    specs
}

fn profiles(spec: &BrowserSpec) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(&spec.root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if spec.firefox {
            found.push(path);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "Default" || name.starts_with("Profile") || name == "Guest Profile" {
            found.push(path);
        }
    }
    found
}

fn history_file(spec: &BrowserSpec, profile: &Path) -> Option<PathBuf> {
    let path = if spec.firefox {
        profile.join("places.sqlite")
    } else {
        profile.join("History")
    };
    path.is_file().then_some(path)
}

fn cookie_files(spec: &BrowserSpec, profile: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if spec.firefox {
        let path = profile.join("cookies.sqlite");
        if path.is_file() {
            files.push(path);
        }
        return files;
    }
    for path in [
        profile.join("Cookies"),
        profile.join("Network").join("Cookies"),
    ] {
        if path.is_file() && !safety::is_password_store(&path.to_string_lossy()) {
            files.push(path);
        }
    }
    files
}

fn connect_copy(path: &Path) -> Option<(Connection, PathBuf)> {
    if !path.is_file() || safety::is_password_store(&path.to_string_lossy()) {
        return None;
    }
    let stamp = timeutil::stamp();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "cdc-privacy-{}-{}-{}.db",
        stamp,
        std::process::id(),
        nonce
    ));
    std::fs::copy(path, &tmp).ok()?;
    let connection = Connection::open(&tmp).ok()?;
    Some((connection, tmp))
}

fn count(connection: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Option<i64> {
    connection
        .query_row(sql, params, |row| row.get::<_, i64>(0))
        .ok()
}

fn domain_count(connection: &Connection, table: &str, domains: &[&str]) -> Option<i64> {
    let clause = domains
        .iter()
        .map(|_| "url LIKE ?")
        .collect::<Vec<_>>()
        .join(" OR ");
    let patterns: Vec<String> = domains
        .iter()
        .map(|domain| format!("%{}%", domain))
        .collect();
    let params: Vec<&dyn rusqlite::ToSql> = patterns
        .iter()
        .map(|pattern| pattern as &dyn rusqlite::ToSql)
        .collect();
    count(
        connection,
        &format!("SELECT COUNT(*) FROM {} WHERE {}", table, clause),
        &params,
    )
}

struct HistoryStats {
    urls: i64,
    downloads: i64,
    shop: i64,
    money: i64,
    medical: i64,
    urls_available: bool,
}

fn history_stats(path: &Path) -> HistoryStats {
    let mut stats = HistoryStats {
        urls: 0,
        downloads: 0,
        shop: 0,
        money: 0,
        medical: 0,
        urls_available: false,
    };
    let Some((connection, tmp)) = connect_copy(path) else {
        return stats;
    };
    let urls = count(&connection, "SELECT COUNT(*) FROM urls", &[])
        .or_else(|| count(&connection, "SELECT COUNT(*) FROM moz_places", &[]));
    if let Some(value) = urls {
        stats.urls = value;
        stats.urls_available = true;
    }
    stats.downloads = count(&connection, "SELECT COUNT(*) FROM downloads", &[])
        .or_else(|| {
            count(
                &connection,
                "SELECT COUNT(*) FROM moz_annos a JOIN moz_anno_attributes n ON a.anno_attribute_id = n.id WHERE n.name LIKE 'downloads/%'",
                &[],
            )
        })
        .unwrap_or(0);
    stats.shop = domain_count(&connection, "urls", &SHOP_DOMAINS)
        .or_else(|| domain_count(&connection, "moz_places", &SHOP_DOMAINS))
        .unwrap_or(0);
    stats.money = domain_count(&connection, "urls", &FINANCE_DOMAINS)
        .or_else(|| domain_count(&connection, "moz_places", &FINANCE_DOMAINS))
        .unwrap_or(0);
    stats.medical = domain_count(&connection, "urls", &MEDICAL_DOMAINS)
        .or_else(|| domain_count(&connection, "moz_places", &MEDICAL_DOMAINS))
        .unwrap_or(0);
    let _ = connection.close();
    let _ = std::fs::remove_file(tmp);
    stats
}

fn cookie_count(path: &Path) -> i64 {
    let Some((connection, tmp)) = connect_copy(path) else {
        return 0;
    };
    let value = count(&connection, "SELECT COUNT(*) FROM cookies", &[])
        .or_else(|| count(&connection, "SELECT COUNT(*) FROM moz_cookies", &[]))
        .unwrap_or(0);
    let _ = connection.close();
    let _ = std::fs::remove_file(tmp);
    value
}

fn sql_statements(kind: &str) -> Vec<String> {
    match kind {
        "history" => vec![
            "DELETE FROM visits".to_string(),
            "DELETE FROM urls".to_string(),
            "DELETE FROM moz_historyvisits".to_string(),
            "DELETE FROM moz_places".to_string(),
        ],
        "cookies" => vec!["DELETE FROM cookies".to_string(), "DELETE FROM moz_cookies".to_string()],
        "downloads" => vec![
            "DELETE FROM downloads_url_chains".to_string(),
            "DELETE FROM downloads".to_string(),
            "DELETE FROM moz_annos WHERE anno_attribute_id IN (SELECT id FROM moz_anno_attributes WHERE name LIKE 'downloads/%')".to_string(),
        ],
        "shop" => domain_sql(&SHOP_DOMAINS),
        "money" => domain_sql(&FINANCE_DOMAINS),
        "medical" => domain_sql(&MEDICAL_DOMAINS),
        _ => Vec::new(),
    }
}

fn domain_sql(domains: &[&str]) -> Vec<String> {
    let likes = domains
        .iter()
        .map(|domain| format!("url LIKE '%{}%'", domain))
        .collect::<Vec<_>>()
        .join(" OR ");
    vec![
        format!(
            "DELETE FROM visits WHERE url IN (SELECT id FROM urls WHERE {})",
            likes
        ),
        format!("DELETE FROM urls WHERE {}", likes),
        format!("DELETE FROM moz_places WHERE {}", likes),
    ]
}

fn apply_sql(path: &Path, statements: &[String]) -> bool {
    if !path.is_file() || safety::is_password_store(&path.to_string_lossy()) {
        return false;
    }
    let Some((connection, tmp)) = connect_copy(path) else {
        return false;
    };
    let mut applied = false;
    for statement in statements {
        if connection.execute_batch(statement).is_ok() {
            applied = true;
        }
    }
    if !applied {
        let _ = connection.close();
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    if connection.close().is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    let copied = std::fs::copy(&tmp, path).is_ok();
    let _ = std::fs::remove_file(&tmp);
    copied
}

// ---------- 注册表 ----------

fn reg_open(root: usize, path: &str, access: u32) -> Option<usize> {
    let name = wide(path);
    let mut key: usize = 0;
    let code = unsafe { RegOpenKeyExW(root, name.as_ptr(), 0, access, &mut key) };
    if code != 0 || key == 0 {
        None
    } else {
        Some(key)
    }
}

fn list_values(key: usize) -> Vec<String> {
    let mut names = Vec::new();
    let mut index = 0u32;
    loop {
        let mut buffer = [0u16; 512];
        let mut length = buffer.len() as u32;
        let code = unsafe {
            RegEnumValueW(
                key,
                index,
                buffer.as_mut_ptr(),
                &mut length,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if code != 0 {
            break;
        }
        names.push(String::from_utf16_lossy(&buffer[..length as usize]));
        index += 1;
    }
    names
}

fn list_subkeys(key: usize) -> Vec<String> {
    let mut names = Vec::new();
    let mut index = 0u32;
    loop {
        let mut buffer = [0u16; 512];
        let mut length = buffer.len() as u32;
        let code = unsafe {
            RegEnumKeyExW(
                key,
                index,
                buffer.as_mut_ptr(),
                &mut length,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if code != 0 {
            break;
        }
        names.push(String::from_utf16_lossy(&buffer[..length as usize]));
        index += 1;
    }
    names
}

fn backup_key(hive: &str, path: &str, label: &str) -> bool {
    let dir = logs::log_dir().join("backups").join("reg");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{}_{}.reg", label, timeutil::stamp()));
    let full = format!("{}\\{}", hive, path);
    let status = Command::new(crate::system32("reg.exe"))
        .args(["export", &full, &file.to_string_lossy(), "/y"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    matches!(status, Ok(status) if status.success())
}

fn clear_values(hive: &str, path: &str, label: &str) -> i64 {
    if !backup_key(hive, path, label) {
        return 0;
    }
    let root = if hive == "HKLM" {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let Some(key) = reg_open(root, path, KEY_ALL_ACCESS) else {
        return 0;
    };
    let names = list_values(key);
    let mut deleted = 0;
    for name in names {
        let wide_name = wide(&name);
        if unsafe { RegDeleteValueW(key, wide_name.as_ptr()) } == 0 {
            deleted += 1;
        }
    }
    unsafe { RegCloseKey(key) };
    deleted
}

fn count_values(hive: &str, path: &str) -> Option<i64> {
    let root = if hive == "HKLM" {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let key = reg_open(root, path, KEY_READ)?;
    let total = list_values(key).len() as i64;
    unsafe { RegCloseKey(key) };
    Some(total)
}

fn count_subkeys(hive: &str, path: &str) -> Option<i64> {
    let root = if hive == "HKLM" {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let key = reg_open(root, path, KEY_READ)?;
    let total = list_subkeys(key).len() as i64;
    unsafe { RegCloseKey(key) };
    Some(total)
}

const RECENT_DOCS: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\RecentDocs";
const USER_ASSIST: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist";
const WMP_RECENT: &str = "Software\\Microsoft\\MediaPlayer\\Player\\RecentFileList";
const USB_STORAGE: &str = "SYSTEM\\CurrentControlSet\\Enum\\USBSTOR";

fn recent_docs_paths() -> Vec<String> {
    let mut paths = vec![RECENT_DOCS.to_string()];
    if let Some(key) = reg_open(HKEY_CURRENT_USER, RECENT_DOCS, KEY_READ) {
        for subkey in list_subkeys(key) {
            paths.push(format!("{}\\{}", RECENT_DOCS, subkey));
        }
        unsafe { RegCloseKey(key) };
    }
    paths
}

fn userassist_count_keys() -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(key) = reg_open(HKEY_CURRENT_USER, USER_ASSIST, KEY_READ) {
        for guid in list_subkeys(key) {
            paths.push(format!("{}\\{}\\Count", USER_ASSIST, guid));
        }
        unsafe { RegCloseKey(key) };
    }
    paths
}

// ---------- 最近文件 / 影音 ----------

fn recent_dir() -> PathBuf {
    PathBuf::from(env_path("APPDATA"))
        .join("Microsoft")
        .join("Windows")
        .join("Recent")
}

fn recent_files() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(recent_dir()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect()
}

// ---------- 扫描 ----------

pub fn scan() -> Vec<PrivacyItem> {
    let specs = browser_specs();

    let mut history_items = Vec::new();
    let mut download_items = Vec::new();
    let mut shop_items = Vec::new();
    let mut money_total = 0i64;
    let mut medical_total = 0i64;
    let mut shop_total = 0i64;
    let mut money_available = false;
    let mut medical_available = false;
    let mut shop_available = false;
    let mut cookie_total = 0i64;
    let mut cookie_available = false;
    let mut cookie_files_all: Vec<String> = Vec::new();

    for spec in &specs {
        let mut urls = 0i64;
        let mut downloads = 0i64;
        let mut shop = 0i64;
        let mut money = 0i64;
        let mut medical = 0i64;
        let mut urls_available = false;
        for profile in profiles(spec) {
            if let Some(history) = history_file(spec, &profile) {
                let stats = history_stats(&history);
                urls += stats.urls;
                downloads += stats.downloads;
                shop += stats.shop;
                money += stats.money;
                medical += stats.medical;
                urls_available |= stats.urls_available;
            }
            for cookie in cookie_files(spec, &profile) {
                let count = cookie_count(&cookie);
                if count > 0 || cookie_available {
                    cookie_available = true;
                }
                cookie_total += count;
                cookie_files_all.push(cookie.to_string_lossy().to_string());
            }
        }
        money_total += money;
        medical_total += medical;
        shop_total += shop;
        money_available |= money > 0;
        medical_available |= medical > 0;
        shop_available |= urls_available;
        history_items.push(PrivacyItem {
            id: format!("hist_{}", spec.id),
            section: "history".to_string(),
            name: spec.name.to_string(),
            count: urls,
            count_available: urls_available,
            files: Vec::new(),
            action: "sql".to_string(),
            note: String::new(),
        });
        download_items.push(PrivacyItem {
            id: format!("down_{}", spec.id),
            section: "download".to_string(),
            name: spec.name.to_string(),
            count: downloads,
            count_available: urls_available,
            files: Vec::new(),
            action: "sql".to_string(),
            note: "只删浏览记录，不删除已下载的文件".to_string(),
        });
        shop_items.push(PrivacyItem {
            id: format!("shop_{}", spec.id),
            section: "shop".to_string(),
            name: spec.name.to_string(),
            count: shop,
            count_available: urls_available,
            files: Vec::new(),
            action: "sql".to_string(),
            note: String::new(),
        });
    }

    let recent = recent_files();
    let sensitive: Vec<String> = recent
        .iter()
        .filter(|path| {
            let name = path
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
            SENSITIVE_WORDS.iter().any(|word| name.contains(word))
        })
        .map(|path| path.to_string_lossy().to_string())
        .collect();

    let mut high = vec![
        PrivacyItem {
            id: "risk_file".to_string(),
            section: "high".to_string(),
            name: "文件敏感隐私".to_string(),
            count: sensitive.len() as i64,
            count_available: !recent.is_empty(),
            files: sensitive,
            action: "files".to_string(),
            note: "最近打开的文件名里含身份证、密码、合同等字样".to_string(),
        },
        PrivacyItem {
            id: "risk_money".to_string(),
            section: "high".to_string(),
            name: "财产安全隐私".to_string(),
            count: money_total,
            count_available: money_available,
            files: Vec::new(),
            action: "sql".to_string(),
            note: String::new(),
        },
        PrivacyItem {
            id: "risk_shop".to_string(),
            section: "high".to_string(),
            name: "网络购物痕迹".to_string(),
            count: shop_total,
            count_available: shop_available,
            files: Vec::new(),
            action: "sql".to_string(),
            note: String::new(),
        },
        PrivacyItem {
            id: "risk_medical".to_string(),
            section: "high".to_string(),
            name: "医疗信息隐私".to_string(),
            count: medical_total,
            count_available: medical_available,
            files: Vec::new(),
            action: "sql".to_string(),
            note: String::new(),
        },
        PrivacyItem {
            id: "cookies".to_string(),
            section: "high".to_string(),
            name: "Cookie".to_string(),
            count: cookie_total,
            count_available: cookie_available,
            files: cookie_files_all,
            action: "sql".to_string(),
            note: "清理后部分网站需要重新登录".to_string(),
        },
    ];
    high.extend(history_items);
    high.extend(download_items);
    high.extend(shop_items);

    let mut computer = Vec::new();
    let recent_docs = recent_docs_paths();
    let mut recent_count = 0i64;
    let mut recent_available = false;
    for path in &recent_docs {
        if let Some(value) = count_values("HKCU", path) {
            recent_available = true;
            recent_count += value;
        }
    }
    computer.push(PrivacyItem {
        id: "reg_file".to_string(),
        section: "computer".to_string(),
        name: "文件访问记录".to_string(),
        count: recent_count,
        count_available: recent_available,
        files: Vec::new(),
        action: "registry".to_string(),
        note: "最近文档列表，删除前会导出备份".to_string(),
    });
    let usb = count_subkeys("HKLM", USB_STORAGE).unwrap_or(0);
    computer.push(PrivacyItem {
        id: "reg_usb".to_string(),
        section: "computer".to_string(),
        name: "USB 使用痕迹".to_string(),
        count: usb,
        count_available: usb > 0,
        files: Vec::new(),
        action: "count".to_string(),
        note: "只统计；清理 USB 记录需要改系统设备记录，Rust 版暂不执行".to_string(),
    });

    let userassist_keys = userassist_count_keys();
    let mut userassist = 0i64;
    for path in &userassist_keys {
        if let Some(value) = count_values("HKCU", path) {
            userassist += value;
        }
    }
    let prefetch = format!("{}\\Prefetch", env_path("SystemRoot"));
    let prefetch_count = std::fs::read_dir(&prefetch)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .to_lowercase()
                        .ends_with(".pf")
                })
                .count() as i64
        })
        .unwrap_or(0);
    let software = vec![PrivacyItem {
        id: "software".to_string(),
        section: "software".to_string(),
        name: "软件使用痕迹".to_string(),
        count: userassist + prefetch_count,
        count_available: true,
        files: Vec::new(),
        action: "userassist".to_string(),
        note: "清理 UserAssist 记录；Prefetch 文件保留".to_string(),
    }];

    let media_files: Vec<String> = recent
        .iter()
        .filter(|path| {
            let stem = path
                .file_stem()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
            let ext = Path::new(&stem)
                .extension()
                .map(|value| format!(".{}", value.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            RECENT_MEDIA_EXT.contains(&ext.as_str())
        })
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    let wmp = count_values("HKCU", WMP_RECENT).unwrap_or(0);
    let vlc = std::fs::read_to_string(
        PathBuf::from(env_path("APPDATA"))
            .join("vlc")
            .join("vlc-qt-interface.ini"),
    )
    .map(|text| {
        text.lines()
            .filter(|line| line.to_lowercase().contains("file:///"))
            .count() as i64
    })
    .unwrap_or(0);
    let media = vec![PrivacyItem {
        id: "media".to_string(),
        section: "media".to_string(),
        name: "影音播放痕迹".to_string(),
        count: media_files.len() as i64 + wmp + vlc,
        count_available: true,
        files: media_files,
        action: "media".to_string(),
        note: "清理最近播放列表和最近打开的音视频快捷方式".to_string(),
    }];

    let mut all = high;
    all.extend(computer);
    all.extend(software);
    all.extend(media);
    all
}

// ---------- 清理 ----------

pub fn clean(ids: Vec<String>) -> PrivacyReport {
    let selected: Vec<PrivacyItem> = scan()
        .into_iter()
        .filter(|item| ids.contains(&item.id))
        .filter(|item| item.action != "count")
        .collect();
    let mut report = PrivacyReport {
        files: 0,
        sql: 0,
        registry: 0,
        failed: Vec::new(),
    };
    let mut files: Vec<String> = Vec::new();
    let mut sql_jobs: Vec<(String, Vec<String>)> = Vec::new();
    let mut registry_jobs: Vec<String> = Vec::new();
    let mut userassist = false;
    let mut media = false;

    for item in &selected {
        match item.action.as_str() {
            "sql" => {
                let kind = item
                    .id
                    .trim_start_matches("hist_")
                    .trim_start_matches("down_")
                    .trim_start_matches("shop_");
                let statements = match item.id.as_str() {
                    "risk_money" => sql_statements("money"),
                    "risk_shop" => sql_statements("shop"),
                    "risk_medical" => sql_statements("medical"),
                    _ if item.id.starts_with("hist_") => sql_statements("history"),
                    _ if item.id.starts_with("down_") => sql_statements("downloads"),
                    _ if item.id.starts_with("shop_") => sql_statements("shop"),
                    _ if item.id == "cookies" => sql_statements("cookies"),
                    _ => Vec::new(),
                };
                let _ = kind;
                if statements.is_empty() {
                    continue;
                }
                for spec in browser_specs() {
                    if item.id != "cookies"
                        && item.id != "risk_money"
                        && item.id != "risk_shop"
                        && item.id != "risk_medical"
                        && !item.id.ends_with(spec.id)
                    {
                        continue;
                    }
                    for profile in profiles(&spec) {
                        if item.id == "cookies" {
                            for cookie in cookie_files(&spec, &profile) {
                                sql_jobs.push((
                                    cookie.to_string_lossy().to_string(),
                                    statements.clone(),
                                ));
                            }
                            continue;
                        }
                        if let Some(history) = history_file(&spec, &profile) {
                            if item.id == "risk_money"
                                || item.id == "risk_shop"
                                || item.id == "risk_medical"
                                || item.id.starts_with("hist_")
                                || item.id.starts_with("down_")
                                || item.id.starts_with("shop_")
                            {
                                sql_jobs.push((
                                    history.to_string_lossy().to_string(),
                                    statements.clone(),
                                ));
                            }
                        }
                    }
                }
            }
            "registry" => registry_jobs.push(item.id.clone()),
            "userassist" => userassist = true,
            "media" => {
                files.extend(item.files.clone());
                media = true;
            }
            _ => files.extend(item.files.clone()),
        }
    }

    files.retain(|path| !path.trim().is_empty() && !safety::is_password_store(path));
    files.sort();
    files.dedup();
    if !files.is_empty() {
        let failed = recycle::send_to_recycle(&files);
        let failed_set: std::collections::HashSet<String> = failed.into_iter().collect();
        for path in &files {
            if failed_set.contains(path) || Path::new(path).exists() {
                report.failed.push(format!("文件未能移入回收站：{}", path));
            } else {
                report.files += 1;
            }
        }
    }

    for (path, statements) in sql_jobs {
        if apply_sql(Path::new(&path), &statements) {
            report.sql += 1;
        } else {
            report
                .failed
                .push(format!("数据库清理失败或数据源不可用：{}", path));
        }
    }

    for job in registry_jobs {
        if job == "reg_file" {
            let mut deleted = 0;
            for path in recent_docs_paths() {
                deleted += clear_values("HKCU", &path, "recentdocs");
            }
            report.registry += deleted;
            if deleted == 0 {
                report
                    .failed
                    .push("文件访问记录清理未完成：没有记录被清除".to_string());
            }
        }
    }

    if userassist {
        let mut deleted = 0;
        for path in userassist_count_keys() {
            deleted += clear_values("HKCU", &path, "userassist");
        }
        report.registry += deleted;
        if deleted == 0 {
            report
                .failed
                .push("软件使用痕迹清理未完成：没有记录被清除".to_string());
        }
    }

    if media {
        report.registry += clear_values("HKCU", WMP_RECENT, "wmp");
    }

    let _ = logs::write_log(&[
        "开始清理".to_string(),
        "清理：隐私痕迹".to_string(),
        format!(
            "完成：文件 {} 个，数据库 {} 个，注册表记录 {} 条",
            report.files, report.sql, report.registry
        ),
    ]);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_statements_cover_both_browsers() {
        let history = sql_statements("history");
        assert!(history.iter().any(|sql| sql.contains("urls")));
        assert!(history.iter().any(|sql| sql.contains("moz_places")));
        let cookies = sql_statements("cookies");
        assert!(cookies.iter().any(|sql| sql.contains("moz_cookies")));
    }

    #[test]
    fn shop_sql_only_targets_fixed_domains() {
        let sql = sql_statements("shop");
        assert!(sql.iter().all(|statement| statement.contains("taobao.com")));
        assert!(sql.iter().all(|statement| !statement.contains("'")) == false);
    }
}
