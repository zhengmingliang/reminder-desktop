//! 提醒的导入与导出：JSON 用来迁移或合并到另一台机器，ICS 用来喂给系统日历。
//! 这一层不碰磁盘，读写文件由调用方（store / 界面）决定。

use chrono::{DateTime, Duration, Local};
use serde::{Deserialize, Serialize};

use crate::model::{Reminder, Schedule};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct ExportFile {
    version: u32,
    exported_at: String,
    reminders: Vec<Reminder>,
}

/// 导出为 JSON。回收站里的条目不带走。
pub fn to_json(reminders: &[Reminder], now: DateTime<Local>) -> Result<String, String> {
    let file = ExportFile {
        version: VERSION,
        exported_at: now.to_rfc3339(),
        reminders: reminders.iter().filter(|item| !item.is_deleted()).cloned().collect(),
    };
    serde_json::to_string_pretty(&file).map_err(|error| format!("生成导出文件失败：{error}"))
}

/// 从 JSON 里读出提醒。版本比程序新就拒绝，和主数据文件一个处理方式。
pub fn from_json(text: &str) -> Result<Vec<Reminder>, String> {
    let file: ExportFile =
        serde_json::from_str(text).map_err(|error| format!("不是有效的导出文件：{error}"))?;
    if file.version > VERSION {
        return Err(format!(
            "导出文件版本是 {}，当前程序只认识版本 {VERSION}",
            file.version
        ));
    }
    Ok(file.reminders)
}

/// 导出为 .ics 文本。只导出还会响的条目：一次性写下一次触发点，
/// 循环提醒同样只写下一个点，cron 表达式放在描述里说明。
pub fn to_ics(reminders: &[Reminder], now: DateTime<Local>) -> String {
    let mut out = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//reminder-desktop//ZH\r\nCALSCALE:GREGORIAN\r\n",
    );
    for item in reminders.iter().filter(|item| !item.is_deleted() && item.enabled) {
        let Some(start) = crate::schedule::next_wake(std::slice::from_ref(item), now) else {
            continue;
        };
        out.push_str("BEGIN:VEVENT\r\n");
        out.push_str(&format!("UID:{}@reminder-desktop\r\n", item.id));
        out.push_str(&format!("DTSTAMP:{}\r\n", utc_stamp(now)));
        out.push_str(&format!("DTSTART:{}\r\n", local_stamp(start)));
        out.push_str(&format!("DTEND:{}\r\n", local_stamp(start + Duration::minutes(15))));
        out.push_str(&format!("SUMMARY:{}\r\n", escape(&item.title)));
        let mut description = item.body.trim().to_string();
        if let Schedule::Cron { expression, .. } = &item.schedule {
            description = if description.is_empty() {
                format!("按 {expression} 重复")
            } else {
                format!("{description}\n按 {expression} 重复")
            };
        }
        if !description.is_empty() {
            out.push_str(&format!("DESCRIPTION:{}\r\n", escape(&description)));
        }
        out.push_str("END:VEVENT\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

/// ICS 里的本地浮动时间：20261020T093000，不带时区。
fn local_stamp(at: DateTime<Local>) -> String {
    at.format("%Y%m%dT%H%M%S").to_string()
}

fn utc_stamp(at: DateTime<Local>) -> String {
    at.to_utc().format("%Y%m%dT%H%M%SZ").to_string()
}

/// ICS 文本转义：反斜杠、分号、逗号、换行都要转义，否则整行会被解析错。
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
        .replace('\r', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Draft, Kind};
    use crate::validate::build_reminder;

    fn one(title: &str) -> Reminder {
        let mut draft = Draft::blank();
        draft.title = title.into();
        draft.minutes = "30".into();
        build_reminder(&draft, None, Local::now(), 0).unwrap()
    }

    fn cron(title: &str) -> Reminder {
        let mut draft = Draft::blank();
        draft.title = title.into();
        draft.kind = Kind::Cron;
        draft.cron = "0 9 * * 1-5".into();
        build_reminder(&draft, None, Local::now(), 0).unwrap()
    }

    #[test]
    fn json_roundtrip_keeps_reminders() {
        let items = vec![one("喝水"), cron("站会")];
        let text = to_json(&items, Local::now()).unwrap();
        let back = from_json(&text).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].title, "喝水");
        assert_eq!(back[1].title, "站会");
    }

    #[test]
    fn json_export_skips_trashed_and_rejects_junk_or_newer_version() {
        let mut trashed = one("删掉的");
        trashed.deleted_at_ms = Some(1_800_000_000_000);
        let text = to_json(&[one("留下"), trashed], Local::now()).unwrap();
        assert_eq!(from_json(&text).unwrap().len(), 1, "回收站里的不该被导出");

        assert!(from_json("not json").is_err());
        let newer = r#"{"version":99,"exported_at":"","reminders":[]}"#;
        assert!(from_json(newer).unwrap_err().contains("版本"));
    }

    #[test]
    fn ics_contains_events_and_escapes_text() {
        let mut item = one("喝水, 记得; 带杯子");
        item.body = "第一行\n第二行".into();
        let text = to_ics(&[item, cron("站会")], Local::now());

        assert!(text.starts_with("BEGIN:VCALENDAR"));
        assert!(text.trim_end().ends_with("END:VCALENDAR"));
        assert_eq!(text.matches("BEGIN:VEVENT").count(), 2);
        assert!(text.contains("DTSTART:"));
        assert!(text.contains("\\,"), "逗号要转义");
        assert!(text.contains("\\;"), "分号要转义");
        assert!(text.contains("\\n"), "换行要转义");
        assert!(text.contains("0 9 * * 1-5"), "循环提醒把 cron 写进描述");

        // 停用和已删除的不该出现在日历里。
        let mut paused = one("停用的");
        paused.enabled = false;
        let text = to_ics(&[paused], Local::now());
        assert!(!text.contains("BEGIN:VEVENT"));
    }
}
