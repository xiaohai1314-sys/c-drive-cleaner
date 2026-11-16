//! 远程规则更新：只接受 https，规则只能是文件清理，且必须落在临时目录或
//! AppData catalog 内；拒绝带命令的规则。对应 Python 版 ruleupdate.py。

use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use crate::safety;

const MAX_BYTES: u64 = 1_000_000;
const MAX_RULES: usize = 200;
const TIERS: [&str; 3] = ["conservative", "standard", "deep"];
const LIBRARY_PARTS: [&str; 6] = ["documents", "desktop", "downloads", "my documents", "文档", "桌面"];

pub fn remote_rules_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(base).join("C盘清理工具");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("rules_remote.json")
}

fn config_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("rules_update.json"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rules_update.json"));
    candidates.into_iter().find(|path| path.is_file())
}

fn config_url() -> String {
    let Some(path) = config_path() else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return String::new();
    };
    value
        .get("url")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 80
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn path_allowed(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("路径为空".to_string());
    }
    if raw.len() > 500 || raw.chars().any(|ch| (ch as u32) < 32) {
        return Err("路径含有非法字符".to_string());
    }
    let expanded = crate::expand_env(raw);
    if safety::path_syntax_error(&expanded) {
        return Err("路径格式不合法".to_string());
    }
    let parts: Vec<String> = expanded
        .replace('/', "\\")
        .to_lowercase()
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect();
    if parts.iter().any(|part| LIBRARY_PARTS.contains(&part.as_str())) {
        return Err("远程规则不能指向文档、桌面或下载".to_string());
    }
    let name = safety::basename(&expanded).to_lowercase();
    if safety::is_password_store(&name) || name.contains("login data") {
        return Err("远程规则不能删除浏览器密码库".to_string());
    }
    if !safety::under_remote_allowlist(&expanded) {
        return Err("远程规则只能指向临时目录或固定 catalog 中的目录".to_string());
    }
    Ok(())
}

fn validate(item: &Value) -> Result<(), String> {
    let Some(object) = item.as_object() else {
        return Err("规则格式不正确".to_string());
    };
    let key = object.get("key").and_then(|value| value.as_str()).unwrap_or("");
    if !key_ok(key) {
        return Err("规则 key 不合法".to_string());
    }
    let kind = object.get("kind").and_then(|value| value.as_str()).unwrap_or("files");
    if kind != "files" {
        return Err(format!("规则 {} 不是文件清理类型", key));
    }
    let tier = object.get("tier").and_then(|value| value.as_str()).unwrap_or("standard");
    if !TIERS.contains(&tier) {
        return Err(format!("规则 {} 的档位不合法", key));
    }
    if let Some(command) = object.get("command") {
        if command.as_array().map(|list| !list.is_empty()).unwrap_or(true) {
            return Err(format!("规则 {} 不能带命令", key));
        }
    }
    for field in ["paths", "glob_dirs"] {
        if let Some(list) = object.get(field) {
            let Some(list) = list.as_array() else {
                return Err(format!("规则 {} 的 {} 格式不正确", key, field));
            };
            for raw in list {
                let Some(text) = raw.as_str() else {
                    return Err(format!("规则 {} 的 {} 含有非字符串", key, field));
                };
                path_allowed(text).map_err(|error| format!("规则 {}：{}", key, error))?;
            }
        }
    }
    Ok(())
}

/// 下载并保存远程规则，返回（新增条数，总条数，说明）。
pub fn update() -> Result<(usize, usize, String), String> {
    let url = config_url();
    if !url.starts_with("https://") {
        return Err("未配置规则更新地址（在 rules_update.json 里填写 https 地址）".to_string());
    }
    let response = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|error| format!("下载失败：{}", error))?;
    let mut text = String::new();
    response
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("读取失败：{}", error))?;
    if text.len() as u64 > MAX_BYTES {
        return Err("规则文件过大".to_string());
    }
    let parsed: Value = serde_json::from_str(&text).map_err(|error| format!("规则格式不正确：{}", error))?;
    let Some(categories) = parsed.get("categories").and_then(|value| value.as_array()) else {
        return Err("规则文件缺少 categories".to_string());
    };
    if categories.len() > MAX_RULES {
        return Err(format!("远程规则过多（上限 {} 条）", MAX_RULES));
    }
    let mut accepted = Vec::new();
    for item in categories {
        validate(item)?;
        accepted.push(item.clone());
    }
    let payload = serde_json::json!({ "version": 1, "categories": accepted });
    let path = remote_rules_path();
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&payload).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("写入失败：{}", error))?;
    let total = crate::all_rules().len();
    Ok((accepted.len(), total, format!("已保存到 {}", path.display())))
}
