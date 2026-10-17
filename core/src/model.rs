use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

pub const TITLE_MAX: usize = 80;
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
    Cron { expression: String },
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
}

impl Reminder {
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
            Schedule::Cron { expression } => (
                Kind::Cron,
                String::new(),
                String::new(),
                String::new(),
                expression.clone(),
            ),
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
        }
    }
}

pub fn millis_to_local(ms: i64) -> DateTime<Local> {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| std::time::UNIX_EPOCH.into())
        .with_timezone(&Local)
}

pub fn now_ms(now: DateTime<Local>) -> i64 {
    now.timestamp_millis()
}
