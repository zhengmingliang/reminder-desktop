//! 用户偏好的持久化：目前只有主题模式，落盘到数据目录下的 settings.json。
//! 与提醒数据分开存，读写都走「先写临时文件再改名」，避免写一半崩溃留坏文件。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SETTINGS_FILE: &str = "settings.json";

/// 主题模式：浅色 / 跟随系统 / 深色。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    System,
    Dark,
}

impl Default for ThemeMode {
    /// 默认跟随系统。
    fn default() -> Self {
        ThemeMode::System
    }
}

impl ThemeMode {
    /// 设置界面里用 0/1/2 传给 Slint（浅色 / 跟随系统 / 深色）。
    pub fn as_index(self) -> i32 {
        match self {
            ThemeMode::Light => 0,
            ThemeMode::System => 1,
            ThemeMode::Dark => 2,
        }
    }

    pub fn from_index(index: i32) -> ThemeMode {
        match index {
            0 => ThemeMode::Light,
            2 => ThemeMode::Dark,
            _ => ThemeMode::System,
        }
    }
}

/// 弹窗一直没被处理时，默认每隔多少分钟再响一次。
pub const DEFAULT_REPEAT_MINUTES: u32 = 5;

fn default_repeat_minutes() -> u32 {
    DEFAULT_REPEAT_MINUTES
}

fn default_hotkey_enabled() -> bool {
    true
}

fn default_dnd_start() -> String {
    "22:00".into()
}

fn default_dnd_end() -> String {
    "07:00".into()
}

/// 勿扰模式：静音弹窗 / 只记录。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DndMode {
    /// 弹窗照弹，但不响铃、不发系统通知。
    Silent,
    /// 不弹不响，只记到概览的「补看」列表。
    Record,
}

fn default_close_to_tray() -> bool {
    true
}

/// 回收站保留天数：默认 30 天，范围 1-365。
pub const DEFAULT_TRASH_KEEP_DAYS: u32 = 30;
pub const TRASH_KEEP_DAYS_MIN: u32 = 1;
pub const TRASH_KEEP_DAYS_MAX: u32 = 365;

fn default_trash_keep_days() -> u32 {
    DEFAULT_TRASH_KEEP_DAYS
}

/// 保留期输入夹在合法范围内，供设置面板校验用。
pub fn clamp_trash_keep_days(value: u32) -> u32 {
    value.clamp(TRASH_KEEP_DAYS_MIN, TRASH_KEEP_DAYS_MAX)
}

impl Default for DndMode {
    /// 默认只记录：勿扰时段最不打断人的做法。
    fn default() -> Self {
        DndMode::Record
    }
}

impl DndMode {
    /// 设置界面里用 0/1 传给 Slint（静音弹窗 / 只记录）。
    pub fn as_index(self) -> i32 {
        match self {
            DndMode::Silent => 0,
            DndMode::Record => 1,
        }
    }

    pub fn from_index(index: i32) -> DndMode {
        match index {
            0 => DndMode::Silent,
            _ => DndMode::Record,
        }
    }
}

/// 列表排列方式：按状态分组（默认），或按下次触发时间分档（今天 / 明天 / 本周 / 更远）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortMode {
    Status,
    Time,
}

impl Default for SortMode {
    fn default() -> Self {
        SortMode::Status
    }
}

impl SortMode {
    pub fn as_index(self) -> i32 {
        match self {
            SortMode::Status => 0,
            SortMode::Time => 1,
        }
    }

    pub fn from_index(index: i32) -> SortMode {
        match index {
            1 => SortMode::Time,
            _ => SortMode::Status,
        }
    }
}

/// 解析 "HH:MM" 为当天分钟数（0..=1439），非法返回 None。
fn parse_hhmm(text: &str) -> Option<u32> {
    let (hour, minute) = text.trim().split_once(':')?;
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    if hour < 24 && minute < 60 {
        Some(hour * 60 + minute)
    } else {
        None
    }
}

/// 文本是否是合法的 "HH:MM" 时间。设置面板输入校验用。
pub fn valid_hhmm(text: &str) -> bool {
    parse_hhmm(text).is_some()
}

/// 此刻是否落在勿扰时段内。支持跨午夜（如 22:00-07:00）；含开始、不含结束。
/// 未启用、起止非法或起止相同，都视为不在勿扰。
pub fn in_quiet_hours(
    now: chrono::DateTime<chrono::Local>,
    enabled: bool,
    start: &str,
    end: &str,
) -> bool {
    use chrono::Timelike;
    if !enabled {
        return false;
    }
    let Some(start) = parse_hhmm(start) else {
        return false;
    };
    let Some(end) = parse_hhmm(end) else {
        return false;
    };
    if start == end {
        return false;
    }
    let minute = now.hour() * 60 + now.minute();
    if start < end {
        (start..end).contains(&minute)
    } else {
        minute >= start || minute < end
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: ThemeMode,
    /// 更新清单地址；留空表示不做更新检查。
    #[serde(default)]
    pub update_url: String,
    /// 弹窗一直没被处理时，每隔多少分钟再响一次；0 表示不再响。
    #[serde(default = "default_repeat_minutes")]
    pub repeat_minutes: u32,
    /// 是否注册全局快捷键（Ctrl+Alt+R 呼出主窗口，Ctrl+Alt+N 快速新建）。仅 X11 会话生效。
    #[serde(default = "default_hotkey_enabled")]
    pub hotkey_enabled: bool,
    /// 勿扰时段开关：时段内到期的提醒按 dnd_mode 处理。
    #[serde(default)]
    pub dnd_enabled: bool,
    /// 勿扰开始时间 "HH:MM"（含）。
    #[serde(default = "default_dnd_start")]
    pub dnd_start: String,
    /// 勿扰结束时间 "HH:MM"（不含）；早于开始时间视为跨午夜。
    #[serde(default = "default_dnd_end")]
    pub dnd_end: String,
    /// 勿扰模式：静音弹窗 / 只记录。
    #[serde(default)]
    pub dnd_mode: DndMode,
    /// 点关闭按钮时的行为：true 隐藏到托盘后台继续提醒，false 直接退出。
    #[serde(default = "default_close_to_tray")]
    pub close_to_tray: bool,
    /// 回收站保留天数，超过后自动彻底删除；1-365。
    #[serde(default = "default_trash_keep_days")]
    pub trash_keep_days: u32,
    /// 列表排列方式：按状态分组 / 按时间分档。
    #[serde(default)]
    pub sort_mode: SortMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            update_url: String::new(),
            repeat_minutes: DEFAULT_REPEAT_MINUTES,
            hotkey_enabled: true,
            dnd_enabled: false,
            dnd_start: default_dnd_start(),
            dnd_end: default_dnd_end(),
            dnd_mode: DndMode::default(),
            close_to_tray: true,
            trash_keep_days: DEFAULT_TRASH_KEEP_DAYS,
            sort_mode: SortMode::Status,
        }
    }
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SETTINGS_FILE)
}

/// 读取设置；文件不存在或内容损坏时回退到默认值（跟随系统）。
pub fn load_settings(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_settings(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(settings).unwrap_or_else(|_| b"{}".to_vec());
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_corrupt_file_falls_back_to_follow_system() {
        let dir = std::env::temp_dir().join(format!("rd-settings-{}", std::process::id()));
        let path = settings_path(&dir);
        assert_eq!(load_settings(&path).theme, ThemeMode::System);
        assert_eq!(load_settings(&path).repeat_minutes, DEFAULT_REPEAT_MINUTES);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load_settings(&path).theme, ThemeMode::System);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 老版本写出来的 settings.json 没有 repeat_minutes，读回来应拿到默认 5 分钟。
    #[test]
    fn older_settings_without_repeat_field_get_the_default() {
        let dir = std::env::temp_dir().join(format!("rd-settings-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = settings_path(&dir);
        std::fs::write(&path, br#"{"theme":"dark","update_url":""}"#).unwrap();
        let loaded = load_settings(&path);
        assert_eq!(loaded.theme, ThemeMode::Dark);
        assert_eq!(loaded.repeat_minutes, DEFAULT_REPEAT_MINUTES);
        assert_eq!(loaded.sort_mode, SortMode::Status, "老设置里没有排列方式，应按状态分组");
        assert!(loaded.hotkey_enabled, "老设置里没有快捷键开关，应默认开启");
        assert!(!loaded.dnd_enabled, "老设置里没有勿扰配置，应默认关闭");
        assert_eq!(loaded.dnd_start, "22:00");
        assert_eq!(loaded.dnd_end, "07:00");
        assert_eq!(loaded.dnd_mode, DndMode::Record);
        assert!(loaded.close_to_tray, "老设置里没有关闭行为开关，应默认隐藏到托盘");
        assert_eq!(loaded.trash_keep_days, DEFAULT_TRASH_KEEP_DAYS, "老设置里没有回收站保留期，应默认 30 天");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_choice_roundtrips_and_uses_temp_rename() {
        let dir = std::env::temp_dir().join(format!("rd-settings-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = settings_path(&dir);
        save_settings(
            &path,
            &Settings {
                theme: ThemeMode::Dark,
                update_url: "https://example.com/latest.json".into(),
                repeat_minutes: 0,
                hotkey_enabled: false,
                dnd_enabled: true,
                dnd_start: "23:30".into(),
                dnd_end: "06:45".into(),
                dnd_mode: DndMode::Silent,
                close_to_tray: false,
                trash_keep_days: 7,
                sort_mode: SortMode::Time,
            },
        )
        .unwrap();
        let loaded = load_settings(&path);
        assert_eq!(loaded.theme, ThemeMode::Dark);
        assert_eq!(loaded.update_url, "https://example.com/latest.json");
        // 关掉（0）也要如实存回来，不能被默认值顶掉。
        assert_eq!(loaded.repeat_minutes, 0);
        assert!(!loaded.hotkey_enabled);
        assert!(loaded.dnd_enabled);
        assert_eq!(loaded.dnd_start, "23:30");
        assert_eq!(loaded.dnd_end, "06:45");
        assert_eq!(loaded.dnd_mode, DndMode::Silent);
        assert!(!loaded.close_to_tray);
        assert_eq!(loaded.trash_keep_days, 7);
        assert_eq!(loaded.sort_mode, SortMode::Time);
        assert_eq!(clamp_trash_keep_days(0), 1, "低于下限要夹回 1");
        assert_eq!(clamp_trash_keep_days(999), TRASH_KEEP_DAYS_MAX, "高于上限要夹回 365");
        assert_eq!(clamp_trash_keep_days(30), 30);
        // 临时文件不应残留。
        assert!(!path.with_extension("json.tmp").exists());
        assert_eq!(ThemeMode::from_index(2), ThemeMode::Dark);
        assert_eq!(ThemeMode::from_index(0).as_index(), 0);
        assert_eq!(DndMode::from_index(0), DndMode::Silent);
        assert_eq!(DndMode::from_index(7).as_index(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn at(hour: u32, minute: u32) -> chrono::DateTime<chrono::Local> {
        use chrono::TimeZone;
        chrono::Local
            .with_ymd_and_hms(2026, 10, 19, hour, minute, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn quiet_hours_disabled_or_invalid_never_matches() {
        assert!(!in_quiet_hours(at(23, 0), false, "22:00", "07:00"));
        assert!(!in_quiet_hours(at(23, 0), true, "22:00", "22:00"));
        assert!(!in_quiet_hours(at(23, 0), true, "25:00", "07:00"));
        assert!(!in_quiet_hours(at(23, 0), true, "22:00", "7:60"));
        assert!(!in_quiet_hours(at(23, 0), true, "", "07:00"));
        assert!(!in_quiet_hours(at(23, 0), true, "22-00", "07:00"));
    }

    #[test]
    fn quiet_hours_same_day_range() {
        // 09:00-12:00：含开始、不含结束。
        assert!(in_quiet_hours(at(9, 0), true, "09:00", "12:00"));
        assert!(in_quiet_hours(at(10, 30), true, "09:00", "12:00"));
        assert!(!in_quiet_hours(at(8, 59), true, "09:00", "12:00"));
        assert!(!in_quiet_hours(at(12, 0), true, "09:00", "12:00"));
        assert!(!in_quiet_hours(at(23, 0), true, "09:00", "12:00"));
    }

    #[test]
    fn quiet_hours_overnight_range() {
        // 22:00-07:00 跨午夜。
        assert!(in_quiet_hours(at(22, 0), true, "22:00", "07:00"));
        assert!(in_quiet_hours(at(23, 59), true, "22:00", "07:00"));
        assert!(in_quiet_hours(at(0, 0), true, "22:00", "07:00"));
        assert!(in_quiet_hours(at(6, 59), true, "22:00", "07:00"));
        assert!(!in_quiet_hours(at(7, 0), true, "22:00", "07:00"));
        assert!(!in_quiet_hours(at(21, 59), true, "22:00", "07:00"));
        assert!(!in_quiet_hours(at(12, 0), true, "22:00", "07:00"));
    }
}
