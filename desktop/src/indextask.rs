//! 计划任务注册（Phase 4）：schtasks.exe + 内嵌 XML 模板。
//!
//! 任务 `\CDriveCleaner\Index`，每日 03:00 跑 `--index --quiet`，当前用户 + S4U +
//! HighestAvailable。注册只在已提权启动时做（后台线程，不阻塞 UI）；失败只记索引日志，
//! 不弹框。UI 只做状态查询 / 手动运行 / 启用禁用 / 注册，不做删除（删除留给卸载流程）。
//!
//! 依据：`~/workspace/goals/c-drive-cleaner/files/schtasks-xml-research.md`。
//! 关键结论：S4U + Highest 可以拿到完全提权 token（微软文档 + Google Security Research
//! #156），但仍需真机验证——见 `register_task` 上的说明与设计文档 §5/§8 Q1。
//! Win32 调用按仓库惯例直接 `extern "system"` 声明（advapi32/kernel32），不为几个函数
//! 引入整套 windows-sys feature。

use std::ffi::c_void;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// 任务全名（`/tn` 用；`\CDriveCleaner` 文件夹由 `/create` 隐式创建）。
pub const TASK_NAME: &str = "\\CDriveCleaner\\Index";
/// 临时 XML 文件名（%TEMP% 下，用完删除）。
const TASK_XML_FILE: &str = "cdrivecleaner-index-task.xml";

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: *mut c_void, desired: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        info: *mut c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *const c_void, out: *mut *mut u16) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

const TOKEN_QUERY: u32 = 0x0000_0008;
/// TOKEN_INFORMATION_CLASS::TokenUser。
const TOKEN_USER_CLASS: u32 = 1;

/// XML 转义（exe 路径 / 用户名代入模板时用；中文路径无须转义）。
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// 月天数（闰年感知），供 `next_0300_date` 做日期进位。
fn days_in_month(year: u16, month: u16) -> u16 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// 下一个 03:00 的日期（纯函数）：现在不到 03:00 取今天，否则取明天。
pub fn next_0300_date(now: crate::timeutil::LocalTime) -> (u16, u16, u16) {
    if now.hour < 3 {
        return (now.year, now.month, now.day);
    }
    let mut year = now.year;
    let mut month = now.month;
    let mut day = now.day + 1;
    if day > days_in_month(year, month) {
        day = 1;
        month += 1;
        if month > 12 {
            month = 1;
            year += 1;
        }
    }
    (year, month, day)
}

/// StartBoundary：下一个 03:00，本地时间，无时区后缀（`YYYY-MM-DDTHH:MM:SS`）。
/// 用未来的时刻——严格等到下一个 03:00，不立即补跑：首次回填在 03:00 静默发生
/// （设计文档 §6）；想立刻建索引用 UI 的"立即同步"（前台跑）或"运行计划任务"
/// （后台跑一次，顺带验证 S4U 提权）。
pub fn next_start_boundary() -> String {
    let now = crate::timeutil::local_time(crate::now_seconds());
    let (year, month, day) = next_0300_date(now);
    format!("{:04}-{:02}-{:02}T03:00:00", year, month, day)
}

/// 任务 XML（模板见调研文档 §5）：`{{EXE_PATH}}` / `{{USER_ID}}` /
/// `{{START_BOUNDARY}}` 由调用方填入，路径与用户名已做 XML 转义。
pub fn build_task_xml(exe_path: &str, user_id: &str, start_boundary: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.3\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <RegistrationInfo>\n    <Author>C盘清理工具</Author>\n    <Description>每日 03:00 维护重复文件 / 大文件索引（--index），空闲时预先算好文件指纹，查询时直接读库。</Description>\n    <Version>1.0</Version>\n  </RegistrationInfo>\n  <Triggers>\n    <CalendarTrigger>\n      <Enabled>true</Enabled>\n      <StartBoundary>{}</StartBoundary>\n      <ScheduleByDay>\n        <DaysInterval>1</DaysInterval>\n      </ScheduleByDay>\n    </CalendarTrigger>\n  </Triggers>\n  <Principals>\n    <Principal id=\"Author\">\n      <UserId>{}</UserId>\n      <LogonType>S4U</LogonType>\n      <RunLevel>HighestAvailable</RunLevel>\n    </Principal>\n  </Principals>\n  <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n    <AllowStartOnDemand>true</AllowStartOnDemand>\n    <StartWhenAvailable>true</StartWhenAvailable>\n    <WakeToRun>false</WakeToRun>\n    <ExecutionTimeLimit>PT2H</ExecutionTimeLimit>\n  </Settings>\n  <Actions Context=\"Author\">\n    <Exec>\n      <Command>{}</Command>\n      <Arguments>--index --quiet</Arguments>\n    </Exec>\n  </Actions>\n</Task>\n",
        start_boundary,
        xml_escape(user_id),
        xml_escape(exe_path),
    )
}

/// 把 XML 写成 UTF-16LE + BOM。schtasks 要求声明与实际字节一致（调研 §4.1）：
/// 声明是 UTF-16 但实际是 UTF-8 时，`/create /xml` 会报误导性的 "Access is denied"。
pub fn write_task_xml_utf16(path: &Path, xml: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(&[0xFF, 0xFE])?; // BOM
    for unit in xml.encode_utf16() {
        file.write_all(&unit.to_le_bytes())?;
    }
    Ok(())
}

/// 当前用户的 SID 字符串（`S-1-5-21-…`）。用户名改名后 SID 不变，任务不会失效
/// （调研 §4.4）。失败返回 None（调用方回退到 `DOMAIN\用户名`）。
pub fn current_user_sid() -> Option<String> {
    unsafe {
        let mut token: *mut c_void = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 || token.is_null() {
            return None;
        }
        let mut len: u32 = 0;
        // 先取所需长度（预期失败并填 len）。
        let _ = GetTokenInformation(token, TOKEN_USER_CLASS, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            CloseHandle(token);
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(
            token,
            TOKEN_USER_CLASS,
            buf.as_mut_ptr() as *mut c_void,
            len,
            &mut len,
        );
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        // TOKEN_USER = { SID_AND_ATTRIBUTES User; }，Sid 在偏移 0 处。
        let sid = *(buf.as_ptr() as *const *const c_void);
        if sid.is_null() {
            return None;
        }
        let mut out: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid, &mut out) == 0 || out.is_null() {
            return None;
        }
        // 返回的是 NUL 结尾的 UTF-16，用 LocalFree 释放。
        let mut units = Vec::new();
        let mut p = out;
        while *p != 0 {
            units.push(*p);
            p = p.add(1);
        }
        let s = String::from_utf16(&units).ok();
        LocalFree(out as *mut c_void);
        s
    }
}

/// 当前用户 `DOMAIN\用户名`（S4U 注册的回退身份；本机账户的 DOMAIN 是计算机名）。
/// 取不到时返回 None。
pub fn current_user_name() -> Option<String> {
    let user = std::env::var("USERNAME").ok()?;
    if user.is_empty() {
        return None;
    }
    let domain = std::env::var("USERDOMAIN")
        .ok()
        .filter(|d| !d.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_default();
    if domain.is_empty() {
        Some(user)
    } else {
        Some(format!("{}\\{}", domain, user))
    }
}

/// 注册用的运行身份：优先 SID（改名不失效），回退 `DOMAIN\用户名`。
pub fn task_user_id() -> Option<String> {
    current_user_sid().or_else(current_user_name)
}

/// 跑一次 schtasks。返回（是否成功，stderr 或 stdout 的首行说明）。
/// 决策只看退出码，不解析输出文本（中文系统上输出是 GBK，UTF-8 解码会乱码）。
fn run_schtasks(args: &[&str]) -> (bool, String) {
    match Command::new("schtasks").args(args).output() {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let text = format!("{}\n{}", stderr.trim(), stdout.trim());
            let first = text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            (out.status.success(), first)
        }
        Err(e) => (false, format!("启动 schtasks 失败：{}", e)),
    }
}

/// 注册（幂等重注册）：写 UTF-16 XML 到 %TEMP%，`schtasks /create /tn … /xml … /f`，
/// 删临时文件。调用方只在已提权时调用——非提权进程注册 Highest 任务直接报
/// 0x80070005（调研 §2.1）。`/f` 整体替换已有任务定义，不需要先 `/delete`。
/// ⚠️ S4U + Highest 是否真提权仍需真机验证（设计文档 §8 Q1）：注册后点 UI 的
/// "运行计划任务"，读 `logs/index-YYYYMMDD.log`，确认走 MFT 路径且 exit 0；
/// 若提权不到（exit 2 或回退遍历），再考虑 SYSTEM + `%ProgramData%` 索引的回退。
pub fn register_task() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("取当前 exe 路径失败：{}", e))?;
    let exe_path = exe.to_string_lossy().into_owned();
    let user_id = task_user_id().ok_or_else(|| "取当前用户身份失败".to_string())?;
    let xml = build_task_xml(&exe_path, &user_id, &next_start_boundary());
    let tmp = std::env::temp_dir().join(TASK_XML_FILE);
    write_task_xml_utf16(&tmp, &xml).map_err(|e| format!("写任务 XML 失败：{}", e))?;
    let (ok, msg) = run_schtasks(&[
        "/create",
        "/tn",
        TASK_NAME,
        "/xml",
        &tmp.to_string_lossy(),
        "/f",
    ]);
    let _ = std::fs::remove_file(&tmp);
    if ok {
        Ok(format!(
            "计划任务已注册：{}（每日 03:00，{}）",
            TASK_NAME, user_id
        ))
    } else if msg.is_empty() {
        Err("schtasks 注册失败（无输出）".to_string())
    } else {
        Err(format!("schtasks 注册失败：{}", msg))
    }
}

/// 任务状态（UI 用）。`/query … /xml` 一次调用同时给存在性与启用状态：
/// 退出码非 0 即任务不存在；导出 XML 里出现 `<Enabled>false</Enabled>` 即被禁用
/// （模板里的触发器 Enabled 恒为 true，false 只可能来自 `/change /disable`；
/// XML 元素名不受系统语言影响，比解析 `/fo list /v` 的本地化文本稳妥）。
/// schtasks 导出的是 UTF-16（带 BOM），先按 UTF-16 解，失败再试 UTF-8。
pub fn task_status() -> (bool, bool) {
    let out = match Command::new("schtasks")
        .args(["/query", "/tn", TASK_NAME, "/xml"])
        .output()
    {
        Ok(out) => out,
        Err(_) => return (false, false),
    };
    if !out.status.success() {
        return (false, false);
    }
    let text = decode_task_xml(&out.stdout);
    (true, !text.contains("<Enabled>false</Enabled>"))
}

fn decode_task_xml(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).unwrap_or_default()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// 手动跑一次（UI 的"运行计划任务"按钮）：立即执行，忽略触发器时间表，
/// 但仍用任务里保存的程序路径、账户和权限。exit 0 只表示"已接受运行请求"，
/// 不表示跑完了——跑完与否看索引日志或 `index_status` 的游标时间。
/// 这也是 S4U 提权的验证入口（见 `register_task` 上的说明）。
pub fn run_task() -> Result<String, String> {
    let (ok, msg) = run_schtasks(&["/run", "/tn", TASK_NAME]);
    if ok {
        Ok("已请求运行计划任务（后台执行，稍后看索引状态与日志）".to_string())
    } else if msg.is_empty() {
        Err("schtasks /run 失败（无输出）".to_string())
    } else {
        Err(format!("schtasks /run 失败：{}", msg))
    }
}

/// 启用 / 禁用（UI 按钮）。禁用后任务不再按触发器运行。
pub fn set_task_enabled(enabled: bool) -> Result<String, String> {
    let flag = if enabled { "/enable" } else { "/disable" };
    let (ok, msg) = run_schtasks(&["/change", "/tn", TASK_NAME, flag]);
    if ok {
        Ok(if enabled {
            "计划任务已启用（每日 03:00）".to_string()
        } else {
            "计划任务已禁用（不再自动同步）".to_string()
        })
    } else if msg.is_empty() {
        Err("schtasks /change 失败（无输出）".to_string())
    } else {
        Err(format!("schtasks /change 失败：{}", msg))
    }
}

/// 删除任务（卸载流程用；UI 不提供按钮，误删会让每日同步停掉）。
/// 注意：删除任务不会删除 `\CDriveCleaner` 文件夹（schtasks 删不掉空文件夹）。
pub fn delete_task() -> Result<String, String> {
    let (ok, msg) = run_schtasks(&["/delete", "/tn", TASK_NAME, "/f"]);
    if ok {
        Ok("计划任务已删除".to_string())
    } else if msg.is_empty() {
        Err("schtasks /delete 失败（无输出）".to_string())
    } else {
        Err(format!("schtasks /delete 失败：{}", msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_special_chars() {
        assert_eq!(xml_escape("C:\\a&b<c>d\"e"), "C:\\a&amp;b&lt;c&gt;d&quot;e");
        assert_eq!(
            xml_escape("C:\\盘清理工具\\app.exe"),
            "C:\\盘清理工具\\app.exe"
        );
    }

    #[test]
    fn build_task_xml_has_template_fields() {
        let xml = build_task_xml(
            "C:\\Program Files\\C盘清理工具\\c-drive-cleaner.exe",
            "S-1-5-21-1-2-3-1001",
            "2026-11-18T03:00:00",
        );
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>"));
        assert!(xml.contains("<Task version=\"1.3\""));
        assert!(xml.contains("<StartBoundary>2026-11-18T03:00:00</StartBoundary>"));
        assert!(xml.contains("<UserId>S-1-5-21-1-2-3-1001</UserId>"));
        assert!(xml.contains("<LogonType>S4U</LogonType>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(
            xml.contains("<Command>C:\\Program Files\\C盘清理工具\\c-drive-cleaner.exe</Command>")
        );
        assert!(xml.contains("<Arguments>--index --quiet</Arguments>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT2H</ExecutionTimeLimit>"));
        assert!(xml.contains("<StartWhenAvailable>true</StartWhenAvailable>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(!xml.contains("{{"));
    }

    #[test]
    fn build_task_xml_escapes_exe_path() {
        let xml = build_task_xml("C:\\a&b\\app.exe", "PC\\user", "2026-11-18T03:00:00");
        assert!(xml.contains("<Command>C:\\a&amp;b\\app.exe</Command>"));
        assert!(xml.contains("<UserId>PC\\user</UserId>"));
    }

    #[test]
    fn next_0300_date_cases() {
        let mk = |year: u16, month: u16, day: u16, hour: u16| crate::timeutil::LocalTime {
            year,
            month,
            day,
            hour,
            minute: 0,
            second: 0,
        };
        // 不到 03:00：今天。
        assert_eq!(next_0300_date(mk(2026, 11, 17, 2)), (2026, 11, 17));
        assert_eq!(next_0300_date(mk(2026, 11, 17, 0)), (2026, 11, 17));
        // 03:00 整点及之后：明天。
        assert_eq!(next_0300_date(mk(2026, 11, 17, 3)), (2026, 11, 18));
        assert_eq!(next_0300_date(mk(2026, 11, 17, 23)), (2026, 11, 18));
        // 月末进位。
        assert_eq!(next_0300_date(mk(2026, 11, 30, 5)), (2026, 12, 1));
        // 年末进位。
        assert_eq!(next_0300_date(mk(2026, 12, 31, 5)), (2027, 1, 1));
        // 闰年 2 月：2028-02-28 → 02-29；2028-02-29 → 03-01。
        assert_eq!(next_0300_date(mk(2028, 2, 28, 5)), (2028, 2, 29));
        assert_eq!(next_0300_date(mk(2028, 2, 29, 5)), (2028, 3, 1));
        // 平年 2 月：2027-02-28 → 03-01。
        assert_eq!(next_0300_date(mk(2027, 2, 28, 5)), (2027, 3, 1));
        // 世纪闰年规则：2000 是闰年，2100 不是。
        assert_eq!(next_0300_date(mk(2000, 2, 28, 5)), (2000, 2, 29));
        assert_eq!(next_0300_date(mk(2100, 2, 28, 5)), (2100, 3, 1));
    }

    #[test]
    fn decode_task_xml_utf16_and_utf8() {
        let xml = "<Task><Enabled>false</Enabled></Task>";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_task_xml(&bytes), xml);
        assert_eq!(decode_task_xml(xml.as_bytes()), xml);
    }
}
