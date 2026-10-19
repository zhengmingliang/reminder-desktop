use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

pub const TITLE_MAX: usize = 80;
/// 标签长度上限。标签是随手打的分类名，长了列表里放不下。
pub const TAG_MAX: usize = 12;
pub const BODY_MAX: usize = 4000;
pub const DELAY_MIN: u32 = 1;
pub const DELAY_MAX: u32 = 525_600;
pub const REMINDER_CAP: usize = 5_000;

/// 三种触发方式。延迟和指定时间是一次性的，Cron 会一直循环。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    Delay { minutes: u32, fire_at_ms: i64 },
    At { fire_at_ms: i64 },
    Cron {
        expression: String,
        /// 结束日期（毫秒）。到这个日期之后不再触发；None 表示一直循环。
        #[serde(default)]
        end_date_ms: Option<i64>,
        /// 最多触发多少次；到次数后标记为已触发。None 表示不限次数。
        #[serde(default)]
        max_occurrences: Option<u32>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Fired,
    Recurring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Delay,
    At,
    Cron,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delay => "delay",
            Self::At => "at",
            Self::Cron => "cron",
        }
    }

    pub fn parse(text: &str) -> Self {
        match text {
            "at" => Self::At,
            "cron" => Self::Cron,
            _ => Self::Delay,
        }
    }
}

/// 表单草稿。字符串保持用户原始输入，方便在校验失败时原样留在输入框里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub id: Option<String>,
    pub title: String,
    pub body: String,
    pub kind: Kind,
    pub minutes: String,
    pub date: String,
    pub time: String,
    pub cron: String,
    pub sound_enabled: bool,
    pub sound_path: String,
    pub enabled: bool,
    /// 提前多少分钟先提示一次；0 表示不提前。
    pub early_minutes: i32,
    /// Cron 结束日期（YYYY-MM-DD），空字符串表示不结束。
    pub cron_end: String,
    /// Cron 最多触发次数，空字符串表示不限。
    pub cron_max: String,
    /// 标签：随手写的分类名，留空表示不分类。
    pub tag: String,
}

impl Draft {
    pub fn blank() -> Self {
        Self {
            id: None,
            title: String::new(),
            body: String::new(),
            kind: Kind::Delay,
            // 新建时就是 1 分钟，和输入框里看到的数字一致，避免空着却像已经填了 1。
            minutes: "1".into(),
            date: String::new(),
            time: String::new(),
            cron: String::new(),
            sound_enabled: true,
            sound_path: String::new(),
            enabled: true,
            early_minutes: 0,
            cron_end: String::new(),
            cron_max: String::new(),
            tag: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    pub title: String,
    pub body: String,
    pub schedule: Schedule,
    pub sound_enabled: bool,
    pub sound_path: Option<String>,
    pub enabled: bool,
    pub status: Status,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_fired_at_ms: Option<i64>,
    /// Cron 最近一次已经处理过的发生点。保存或重新启用时设为“现在”，避免把创建前的周期补报出来。
    pub last_occurrence_ms: Option<i64>,
    /// 用户在弹窗里选择了稍后提醒。到点前压住原来的计划，到点后单独再响一次。
    #[serde(default)]
    pub snooze_until_ms: Option<i64>,
    /// 已经按过几次稍后提醒。再次弹出时用来显示「第 N 次」。
    #[serde(default)]
    pub snooze_count: u32,
    /// 到点前多少分钟先弹一次提示；0 表示不提前。
    #[serde(default)]
    pub early_minutes: u32,
    /// 已经对哪个触发点（毫秒）发过提前提醒，避免同一个点重复弹。
    #[serde(default)]
    pub early_notice_sent_ms: Option<i64>,
    /// Cron 已经触发的次数（用于 max_occurrences 判断）。
    #[serde(default)]
    pub occurrence_count: u32,
    /// 要跳过的触发点（毫秒）。到点时不弹窗，只把计划推进过这一点。
    #[serde(default)]
    pub skip_occurrence_ms: Option<i64>,
    /// 最近一次在弹窗里点「知道了」的时间（毫秒）。用来区分「看过了」和「被关掉」。
    #[serde(default)]
    pub acknowledged_at_ms: Option<i64>,
    /// 移入回收站的时间（毫秒）。None 表示还在正常列表里；回收站保留期到后彻底清除。
    #[serde(default)]
    pub deleted_at_ms: Option<i64>,
    /// 标签：列表色条、弹窗色标与筛选都用它；None 表示不分类。
    #[serde(default)]
    pub tag: Option<String>,
}

impl Reminder {
    /// 是否已移入回收站。调度、列表、统计都要跳过这类条目。
    pub fn is_deleted(&self) -> bool {
        self.deleted_at_ms.is_some()
    }

    /// 是否在某一天被确认过。「今日已完成」用它挑当天处理掉的条目。
    pub fn acknowledged_on(&self, day: DateTime<Local>) -> bool {
        self.acknowledged_at_ms
            .is_some_and(|ms| millis_to_local(ms).date_naive() == day.date_naive())
    }

    pub fn to_draft(&self) -> Draft {
        let (kind, minutes, date, time, cron) = match &self.schedule {
            Schedule::Delay { minutes, .. } => {
                (Kind::Delay, minutes.to_string(), String::new(), String::new(), String::new())
            }
            Schedule::At { fire_at_ms } => {
                let dt = millis_to_local(*fire_at_ms);
                (
                    Kind::At,
                    String::new(),
                    dt.format("%Y-%m-%d").to_string(),
                    dt.format("%H:%M").to_string(),
                    String::new(),
                )
            }
            Schedule::Cron { expression, .. } => (
                Kind::Cron,
                String::new(),
                String::new(),
                String::new(),
                expression.clone(),
            ),
        };
        let (cron_end, cron_max) = match &self.schedule {
            Schedule::Cron {
                end_date_ms,
                max_occurrences,
                ..
            } => (
                end_date_ms
                    .map(|ms| millis_to_local(ms).format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                max_occurrences.map(|n| n.to_string()).unwrap_or_default(),
            ),
            _ => (String::new(), String::new()),
        };
        Draft {
            id: Some(self.id.clone()),
            title: self.title.clone(),
            body: self.body.clone(),
            kind,
            minutes,
            date,
            time,
            cron,
            sound_enabled: self.sound_enabled,
            sound_path: self.sound_path.clone().unwrap_or_default(),
            // 已触发的一次性提醒打开后默认勾选启用，直接保存就会重新开始。
            enabled: self.enabled || self.status == Status::Fired,
            early_minutes: i32::try_from(self.early_minutes).unwrap_or(0),
            cron_end,
            cron_max,
            tag: self.tag.clone().unwrap_or_default(),
        }
    }

    /// 标签文本；没有标签是空串，界面据此决定要不要画色条。
    pub fn tag_text(&self) -> &str {
        self.tag.as_deref().unwrap_or("")
    }
}

/// 标签配色：颜色跟着标签文本走，同样的文本永远得到同一个颜色，不用单独挑色。
pub const TAG_COLORS: [&str; 6] =
    ["#2E7D74", "#B4701F", "#6B4FA0", "#B42318", "#067647", "#175CD3"];

/// 取标签颜色；没有标签（空文本）返回空串，界面据此决定要不要画色条。
pub fn tag_color(tag: &str) -> &'static str {
    if tag.trim().is_empty() {
        return "";
    }
    let sum: usize = tag.chars().map(|character| u32::from(character) as usize).sum();
    TAG_COLORS[sum % TAG_COLORS.len()]
}

/// 标签颜色的 RGB 分量，界面直接拿来画色条；没有标签时返回 None。
pub fn tag_rgb(tag: &str) -> Option<(u8, u8, u8)> {
    let hex = tag_color(tag);
    if hex.is_empty() {
        return None;
    }
    let value = u32::from_str_radix(hex.trim_start_matches('#'), 16).ok()?;
    Some(((value >> 16) as u8, (value >> 8) as u8, value as u8))
}

pub fn millis_to_local(ms: i64) -> DateTime<Local> {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| std::time::UNIX_EPOCH.into())
        .with_timezone(&Local)
}

pub fn now_ms(now: DateTime<Local>) -> i64 {
    now.timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标签取色：同样文本永远得到同一个颜色，没有标签就不给颜色。
    #[test]
    fn tag_color_is_stable_and_absent_when_empty() {
        assert_eq!(tag_color("工作"), tag_color("工作"));
        assert_eq!(tag_color(""), "");
        assert_eq!(tag_color("   "), "");
        assert!(tag_rgb("").is_none());
        let rgb = tag_rgb("工作").expect("有标签就该有颜色");
        assert_eq!(tag_rgb("工作"), Some(rgb), "同一个标签颜色必须稳定");
        assert!(TAG_COLORS.contains(&tag_color("工作")), "颜色要取自预设配色");
    }

    /// 老版本 reminders.json 没有 tag 字段，读进来应是不分类。
    #[test]
    fn older_reminder_without_tag_defaults_to_no_tag() {
        let raw = serde_json::json!({
            "id": "c",
            "title": "喝水",
            "body": "",
            "schedule": {"kind": "delay", "minutes": 5, "fire_at_ms": 1_800_000_000_000i64},
            "sound_enabled": true,
            "sound_path": null,
            "enabled": true,
            "status": "pending",
            "created_at_ms": 0,
            "updated_at_ms": 0,
            "last_fired_at_ms": null,
            "last_occurrence_ms": null
        });
        let reminder: Reminder = serde_json::from_value(raw).unwrap();
        assert_eq!(reminder.tag, None);
        assert_eq!(reminder.tag_text(), "");
        assert_eq!(reminder.to_draft().tag, "");
    }

    /// 老版本 reminders.json 没有 early_minutes / early_notice_sent_ms，读进来应是不提前。
    #[test]
    fn older_reminder_without_early_fields_defaults_to_no_early_notice() {
        let raw = serde_json::json!({
            "id": "a",
            "title": "喝水",
            "body": "",
            "schedule": {"kind": "at", "fire_at_ms": 1_800_000_000_000i64},
            "sound_enabled": true,
            "sound_path": null,
            "enabled": true,
            "status": "pending",
            "created_at_ms": 0,
            "updated_at_ms": 0,
            "last_fired_at_ms": null,
            "last_occurrence_ms": null
        });
        let reminder: Reminder = serde_json::from_value(raw).unwrap();
        assert_eq!(reminder.early_minutes, 0);
        assert_eq!(reminder.early_notice_sent_ms, None);
        assert_eq!(reminder.skip_occurrence_ms, None);
        assert_eq!(reminder.to_draft().early_minutes, 0);
    }

    /// 老版本 Cron 没有 end_date_ms / max_occurrences / occurrence_count，读进来应是不限。
    #[test]
    fn older_cron_without_end_fields_defaults_to_unlimited() {
        let raw = serde_json::json!({
            "id": "b",
            "title": "站会",
            "body": "",
            "schedule": {"kind": "cron", "expression": "0 9 * * 1-5"},
            "sound_enabled": true,
            "sound_path": null,
            "enabled": true,
            "status": "recurring",
            "created_at_ms": 0,
            "updated_at_ms": 0,
            "last_fired_at_ms": null,
            "last_occurrence_ms": null
        });
        let reminder: Reminder = serde_json::from_value(raw).unwrap();
        match &reminder.schedule {
            Schedule::Cron {
                end_date_ms,
                max_occurrences,
                ..
            } => {
                assert_eq!(*end_date_ms, None);
                assert_eq!(*max_occurrences, None);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(reminder.occurrence_count, 0);
        assert_eq!(reminder.to_draft().cron_end, "");
        assert_eq!(reminder.to_draft().cron_max, "");
    }
}
