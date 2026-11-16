//! 本地时间换算：把 Unix 秒转成 Windows SYSTEMTIME，供日志和聊天月份分组使用。

#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn FileTimeToLocalFileTime(file_time: *const FileTime, local: *mut FileTime) -> i32;
    fn FileTimeToSystemTime(file_time: *const FileTime, system_time: *mut SystemTimeRaw) -> i32;
}

#[repr(C)]
#[derive(Default)]
struct SystemTimeRaw {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

pub fn local_time(seconds: u64) -> LocalTime {
    // FILETIME 从 1601-01-01 起算，单位 100ns。
    let ticks = (seconds.saturating_add(11_644_473_600)).saturating_mul(10_000_000);
    let file_time = FileTime {
        low: (ticks & 0xFFFF_FFFF) as u32,
        high: (ticks >> 32) as u32,
    };
    let mut local = FileTime { low: 0, high: 0 };
    let mut system = SystemTimeRaw::default();
    unsafe {
        if FileTimeToLocalFileTime(&file_time, &mut local) == 0
            || FileTimeToSystemTime(&local, &mut system) == 0
        {
            return LocalTime::default();
        }
    }
    LocalTime {
        year: system.year,
        month: system.month,
        day: system.day,
        hour: system.hour,
        minute: system.minute,
        second: system.second,
    }
}

pub fn month_label(seconds: u64) -> String {
    let time = local_time(seconds);
    if time.year == 0 {
        return "1970-01".to_string();
    }
    format!("{:04}-{:02}", time.year, time.month)
}

pub fn stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let time = local_time(seconds);
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    )
}

pub fn human_stamp(seconds: u64) -> String {
    let time = local_time(seconds);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    )
}
