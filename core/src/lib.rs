//! 桌面提醒的纯逻辑层：模型、校验、到期计算、磁盘存储、铃声解码。
//! 界面进程只调用这里，方便单独测试，也避免以后换 UI 时改调度规则。

pub mod audio;
pub mod cli;
pub mod model;
pub mod schedule;
pub mod settings;
pub mod store;
pub mod update;
pub mod validate;

pub use audio::{check_sound_file, decode_bytes, read_sound, AudioSource, DEFAULT_CHIME};
pub use model::{millis_to_local, Draft, Kind, Reminder, Schedule, Status};
pub use schedule::{
    center_on_monitor, due_reminders, early_reminders, format_point, list_texts, next_occurrence_ms,
    next_wake, recent_records, skipped_reminders, today_timeline, DueHit, EarlyHit, ListText,
    RecordEntry, SkippedHit, TimelineEntry,
};
pub use store::{default_data_dir, Store};
pub use settings::{load_settings, save_settings, settings_path, Settings, ThemeMode};
pub use validate::{build_reminder, inspect, next_cron_times, FieldError, Report};
