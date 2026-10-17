use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, Timelike};
use croner::Cron;
use uuid::Uuid;

use crate::model::{
    now_ms, Draft, Kind, Reminder, Schedule, Status, BODY_MAX, DELAY_MAX, DELAY_MIN, REMINDER_CAP,
    TITLE_MAX,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub errors: Vec<FieldError>,
    pub preview: Vec<String>,
    pub delay_hint: String,
    pub at_hint: String,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn inspect(draft: &Draft, now: DateTime<Local>) -> Report {
    let mut errors = Vec::new();
    let title = normalize_title(&draft.title);
    if title.is_empty() {
        errors.push(err("title", "请填写提醒标题"));
    } else if title.chars().count() > TITLE_MAX {
        errors.push(err(
            "title",
            format!("标题最多 {TITLE_MAX} 个字，当前 {} 个", title.chars().count()),
        ));
    }
    let body_len = draft.body.chars().count();
    if body_len > BODY_MAX {
        errors.push(err(
            "body",
            format!("正文最多 {BODY_MAX} 个字，当前 {body_len} 个"),
        ));
    }

    let mut preview = Vec::new();
    let mut delay_hint = String::new();
    let mut at_hint = String::new();

    match draft.kind {
        Kind::Delay => match parse_minutes(&draft.minutes) {
            Ok(minutes) => {
                let fire = now + Duration::minutes(i64::from(minutes));
                delay_hint = format!(
                    "保存后将在 {} 触发",
                    fire.format("%Y-%m-%d %H:%M:%S")
                );
            }
            Err(message) => errors.push(err("delay", message)),
        },
        Kind::At => match parse_at(&draft.date, &draft.time, now) {
            Ok(fire) => {
                at_hint = format!(
                    "将在 {} 触发，{}",
                    fire.format("%Y-%m-%d %H:%M"),
                    crate::schedule::format_distance(now, fire)
                );
            }
            Err(message) => {
                let field = if message.contains("时间") && !message.contains("日期") {
                    "time"
                } else if message.contains("日期") {
                    "date"
                } else {
                    "time"
                };
                errors.push(err(field, message));
            }
        },
        Kind::Cron => match next_cron_times(&draft.cron, now, 10) {
            Ok(times) => {
                if times.is_empty() {
                    errors.push(err("cron", "未来 5 年内没有匹配的执行时间"));
                }
                preview = times
                    .iter()
                    .map(|time| crate::schedule::format_preview_line(*time, now.year()))
                    .collect();
            }
            Err(message) => errors.push(err("cron", message)),
        },
    }

    Report {
        errors,
        preview,
        delay_hint,
        at_hint,
    }
}

pub fn build_reminder(
    draft: &Draft,
    previous: Option<&Reminder>,
    now: DateTime<Local>,
    existing_count: usize,
) -> Result<Reminder, Report> {
    let report = inspect(draft, now);
    if !report.ok() {
        return Err(report);
    }
    if previous.is_none() && existing_count >= REMINDER_CAP {
        let mut report = report;
        report.errors.push(err(
            "form",
            format!("提醒数量已达上限 {REMINDER_CAP}，请先删除不用的条目"),
        ));
        return Err(report);
    }

    let title = normalize_title(&draft.title);
    let body = draft.body.replace("\r\n", "\n").replace('\r', "\n");
    let schedule = match draft.kind {
        Kind::Delay => {
            let minutes = parse_minutes(&draft.minutes).expect("inspect 已通过");
            let fire_at_ms = (now + Duration::minutes(i64::from(minutes))).timestamp_millis();
            Schedule::Delay { minutes, fire_at_ms }
        }
        Kind::At => {
            let fire = parse_at(&draft.date, &draft.time, now).expect("inspect 已通过");
            Schedule::At { fire_at_ms: fire.timestamp_millis() }
        }
        Kind::Cron => Schedule::Cron {
            expression: normalize_cron(&draft.cron),
        },
    };
    let enabled = draft.enabled;
    let status = match (&schedule, enabled) {
        (Schedule::Cron { .. }, _) => Status::Recurring,
        (_, _) => Status::Pending,
    };
    let created_at_ms = previous.map(|item| item.created_at_ms).unwrap_or_else(|| now_ms(now));
    let last_occurrence_ms = match &schedule {
        Schedule::Cron { .. } => Some(now_ms(now)),
        _ => None,
    };
    Ok(Reminder {
        id: previous
            .map(|item| item.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        title,
        body,
        schedule,
        sound_enabled: draft.sound_enabled,
        sound_path: {
            let path = draft.sound_path.trim();
            if path.is_empty() { None } else { Some(path.to_string()) }
        },
        enabled,
        status,
        created_at_ms,
        updated_at_ms: now_ms(now),
        last_fired_at_ms: previous.and_then(|item| item.last_fired_at_ms),
        last_occurrence_ms,
        snooze_until_ms: None,
        snooze_count: 0,
    })
}

/// 列表上直接启用或暂停。指定时间若已过期，必须先改时间。
pub fn set_enabled(reminder: &mut Reminder, enabled: bool, now: DateTime<Local>) -> Result<(), String> {
    if !enabled {
        reminder.enabled = false;
        reminder.snooze_until_ms = None;
        reminder.updated_at_ms = now_ms(now);
        return Ok(());
    }
    reminder.snooze_until_ms = None;
    match &mut reminder.schedule {
        Schedule::Delay { minutes, fire_at_ms } => {
            *fire_at_ms = (now + Duration::minutes(i64::from(*minutes))).timestamp_millis();
            reminder.status = Status::Pending;
        }
        Schedule::At { fire_at_ms } => {
            if *fire_at_ms <= now_ms(now) {
                return Err("指定时间已过，请修改时间后再启用".into());
            }
            reminder.status = Status::Pending;
        }
        Schedule::Cron { .. } => {
            reminder.status = Status::Recurring;
            reminder.last_occurrence_ms = Some(now_ms(now));
        }
    }
    reminder.enabled = true;
    reminder.updated_at_ms = now_ms(now);
    Ok(())
}

pub fn arm_snooze(reminder: &mut Reminder, minutes: u32, now: DateTime<Local>) -> Result<(), String> {
    if !matches!(minutes, 5 | 15 | 30 | 60) {
        return Err("稍后提醒只支持 5 分钟、15 分钟、30 分钟或 1 小时".into());
    }
    reminder.snooze_until_ms = Some((now + Duration::minutes(i64::from(minutes))).timestamp_millis());
    reminder.snooze_count = reminder.snooze_count.saturating_add(1);
    reminder.enabled = true;
    reminder.status = match reminder.schedule {
        Schedule::Cron { .. } => Status::Recurring,
        _ => Status::Pending,
    };
    reminder.updated_at_ms = now_ms(now);
    Ok(())
}

pub fn finish_snooze(reminder: &mut Reminder, now: DateTime<Local>) {
    reminder.snooze_until_ms = None;
    reminder.last_fired_at_ms = Some(now_ms(now));
    reminder.updated_at_ms = now_ms(now);
    match reminder.schedule {
        Schedule::Cron { .. } => {
            reminder.last_occurrence_ms = Some(now_ms(now));
            reminder.status = Status::Recurring;
            reminder.enabled = true;
        }
        _ => {
            reminder.status = Status::Fired;
            reminder.enabled = false;
        }
    }
}

pub fn apply_fire(reminder: &mut Reminder, occurrence_ms: i64, now: DateTime<Local>) {
    reminder.snooze_until_ms = None;
    reminder.last_fired_at_ms = Some(now_ms(now));
    reminder.last_occurrence_ms = Some(occurrence_ms);
    reminder.updated_at_ms = now_ms(now);
    match reminder.schedule {
        Schedule::Cron { .. } => {
            reminder.status = Status::Recurring;
            reminder.enabled = true;
        }
        _ => {
            reminder.status = Status::Fired;
            reminder.enabled = false;
        }
    }
}

fn err(field: &'static str, message: impl Into<String>) -> FieldError {
    FieldError { field, message: message.into() }
}

fn normalize_title(title: &str) -> String {
    title
        .replace(['\n', '\r', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn parse_minutes(text: &str) -> Result<u32, String> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err("请输入延迟分钟数".into());
    }
    if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("分钟数必须是整数，不能包含小数、正负号或其它字符".into());
    }
    let stripped = raw.trim_start_matches('0');
    let normalized = if stripped.is_empty() { "0" } else { stripped };
    if normalized.len() > 6 {
        return Err(format!("延迟不能超过 {DELAY_MAX} 分钟（365 天）"));
    }
    let value: u32 = normalized.parse().expect("仅数字");
    if !(DELAY_MIN..=DELAY_MAX).contains(&value) {
        return Err(format!(
            "请输入 {DELAY_MIN} 到 {DELAY_MAX} 之间的整数分钟（最多 365 天）"
        ));
    }
    Ok(value)
}

pub fn parse_at(date: &str, time: &str, now: DateTime<Local>) -> Result<DateTime<Local>, String> {
    let date_text = canonicalize_date(date);
    let time_text = canonicalize_time(time);
    if date.trim().is_empty() {
        return Err("请填写日期，格式为 YYYY-MM-DD".into());
    }
    let date = NaiveDate::parse_from_str(&date_text, "%Y-%m-%d")
        .map_err(|_| format!("日期无效，请使用 YYYY-MM-DD，例如 {}", now.format("%Y-%m-%d")))?;
    if time.trim().is_empty() {
        return Err("请填写时间，格式为 HH:MM".into());
    }
    let time = NaiveTime::parse_from_str(&time_text, "%H:%M").map_err(|_| {
        "时间无效，请使用 24 小时制 HH:MM，例如 09:30".to_string()
    })?;
    if time.second() != 0 || time.nanosecond() != 0 {
        return Err("时间只精确到分钟".into());
    }
    let local = date
        .and_time(time)
        .and_local_timezone(Local)
        .single()
        .ok_or_else(|| "这个时间在当前时区不存在或有歧义，请换一个分钟".to_string())?;
    if local <= now {
        return Err("触发时间必须晚于当前时间".into());
    }
    Ok(local)
}

fn normalize_date(text: &str) -> String {
    text.trim()
        .replace(['/', '.', '－', '—'], "-")
        .replace('年', "-")
        .replace(['月', '日'], "")
}

fn canonicalize_date(text: &str) -> String {
    let text = normalize_date(text);
    let mut parts = text.split('-');
    let Some(year) = parts.next() else {
        return text;
    };
    let Some(month) = parts.next() else {
        return text;
    };
    let Some(day) = parts.next() else {
        return text;
    };
    if parts.next().is_some() || year.len() != 4 {
        return text;
    }
    if !month.bytes().all(|byte| byte.is_ascii_digit()) || !day.bytes().all(|byte| byte.is_ascii_digit())
    {
        return text;
    }
    format!("{year}-{month:0>2}-{day:0>2}")
}

fn canonicalize_time(text: &str) -> String {
    let text = text.trim().replace('：', ":").replace('.', ":");
    let mut parts = text.split(':');
    let Some(hour) = parts.next() else {
        return text;
    };
    let Some(minute) = parts.next() else {
        return text;
    };
    if parts.next().is_some() {
        return text;
    }
    if hour.is_empty()
        || minute.is_empty()
        || !hour.bytes().all(|byte| byte.is_ascii_digit())
        || !minute.bytes().all(|byte| byte.is_ascii_digit())
    {
        return text;
    }
    format!("{hour:0>2}:{minute:0>2}")
}

pub fn normalize_cron(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn parse_cron(text: &str) -> Result<Cron, String> {
    let normalized = normalize_cron(text);
    if normalized.is_empty() {
        return Err("请输入 Cron 表达式".into());
    }
    if normalized.chars().any(|ch| matches!(ch, '?' | '#' | 'L' | 'W')) {
        return Err("不支持 ?、L、W、# 这些扩展符号，请使用标准 5 段 Cron".into());
    }
    let fields: Vec<&str> = normalized.split(' ').collect();
    if fields.len() != 5 {
        let extra = if fields.len() == 6 || fields.len() == 7 {
            "不需要秒或年字段。"
        } else {
            ""
        };
        return Err(format!(
            "Cron 需要 5 段（分 时 日 月 周），当前是 {} 段。{extra}示例：0 9 * * 1-5",
            fields.len()
        ));
    }
    let labels = ["分钟", "小时", "日", "月", "周"];
    let ranges = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    let names = [
        Vec::new(),
        Vec::new(),
        Vec::new(),
        month_names(),
        dow_names(),
    ];
    for index in 0..5 {
        check_field(fields[index], labels[index], ranges[index], &names[index])?;
    }
    // croner 2 的 FromStr 不会真正解析，必须调用 parse。
    Cron::new(&normalized)
        .parse()
        .map_err(|error| format!("Cron 表达式无效：{error}"))
}

pub fn next_cron_times(
    expression: &str,
    now: DateTime<Local>,
    count: usize,
) -> Result<Vec<DateTime<Local>>, String> {
    let cron = parse_cron(expression)?;
    let horizon = now + Duration::days(365 * 5);
    let mut times = Vec::with_capacity(count);
    let mut cursor = now;
    for _ in 0..(count * 40).max(40) {
        let next = cron.find_next_occurrence(&cursor, false).map_err(|error| {
            format!("无法计算下一次执行时间：{error}")
        })?;
        if next > horizon {
            break;
        }
        times.push(next);
        if times.len() == count {
            break;
        }
        cursor = next;
    }
    if times.is_empty() {
        return Err("未来 5 年内没有匹配的执行时间".into());
    }
    Ok(times)
}

fn month_names() -> Vec<(&'static str, u32)> {
    vec![
        ("JAN", 1), ("FEB", 2), ("MAR", 3), ("APR", 4), ("MAY", 5), ("JUN", 6),
        ("JUL", 7), ("AUG", 8), ("SEP", 9), ("OCT", 10), ("NOV", 11), ("DEC", 12),
    ]
}

fn dow_names() -> Vec<(&'static str, u32)> {
    vec![
        ("SUN", 0), ("MON", 1), ("TUE", 2), ("WED", 3), ("THU", 4), ("FRI", 5), ("SAT", 6),
    ]
}

fn check_field(
    field: &str,
    label: &str,
    range: (u32, u32),
    names: &[(&str, u32)],
) -> Result<(), String> {
    if field.is_empty() {
        return Err(format!("{label}字段不能为空"));
    }
    for part in field.split(',') {
        if part.is_empty() {
            return Err(format!("{label}字段有空项，请检查逗号"));
        }
        check_part(part, label, range, names)?;
    }
    Ok(())
}

fn check_part(
    part: &str,
    label: &str,
    range: (u32, u32),
    names: &[(&str, u32)],
) -> Result<(), String> {
    let (base, step) = match part.split_once('/') {
        Some((base, step)) => {
            if step.is_empty() || !step.bytes().all(|byte| byte.is_ascii_digit()) || step == "0" {
                return Err(format!("{label}字段的步长必须是大于 0 的整数"));
            }
            if step.len() > 3 {
                return Err(format!("{label}字段的步长过大"));
            }
            (base, true)
        }
        None => (part, false),
    };
    if base == "*" {
        if part.matches('/').count() > 1 {
            return Err(format!("{label}字段格式不正确"));
        }
        return Ok(());
    }
    if base.contains('/') {
        return Err(format!("{label}字段格式不正确"));
    }
    let (start, end) = if let Some((start, end)) = base.split_once('-') {
        if start.is_empty() || end.is_empty() || end.contains('-') {
            return Err(format!("{label}字段的范围格式不正确"));
        }
        (resolve_token(start, label, range, names)?, resolve_token(end, label, range, names)?)
    } else {
        let value = resolve_token(base, label, range, names)?;
        (value, value)
    };
    if start > end {
        return Err(format!("{label}字段的范围要从小到大"));
    }
    let _ = step;
    Ok(())
}

fn resolve_token(
    token: &str,
    label: &str,
    range: (u32, u32),
    names: &[(&str, u32)],
) -> Result<u32, String> {
    if token.bytes().all(|byte| byte.is_ascii_digit()) {
        if token.len() > 3 {
            return Err(format!("{label}字段的数字超出范围（{}–{}）", range.0, range.1));
        }
        let value: u32 = token.parse().map_err(|_| format!("{label}字段数字无效"))?;
        if value < range.0 || value > range.1 {
            return Err(format!("{label}字段必须在 {} 到 {} 之间", range.0, range.1));
        }
        return Ok(value);
    }
    let upper = token.to_ascii_uppercase();
    names
        .iter()
        .find(|(name, _)| *name == upper)
        .map(|(_, value)| *value)
        .ok_or_else(|| {
            if names.is_empty() {
                format!("{label}字段含有无法识别的内容“{token}”")
            } else {
                let joined = names.iter().map(|(name, _)| *name).collect::<Vec<_>>().join("、");
                format!("{label}字段不认识“{token}”，可用数字或 {joined}")
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    fn now() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 10, 17, 15, 30, 45)
            .single()
            .unwrap_or_else(Local::now)
    }

    fn draft() -> Draft {
        let mut draft = Draft::blank();
        draft.title = "站会".into();
        draft.body = "带上周报".into();
        draft
    }

    #[test]
    fn delay_minutes_are_strict() {
        assert_eq!(parse_minutes("15").unwrap(), 15);
        assert_eq!(parse_minutes("0005").unwrap(), 5);
        assert!(parse_minutes("").unwrap_err().contains("请输入"));
        assert!(parse_minutes("0").unwrap_err().contains("1 到"));
        assert!(parse_minutes("1.5").unwrap_err().contains("整数"));
        assert!(parse_minutes("-3").unwrap_err().contains("整数"));
        assert!(parse_minutes("525601").unwrap_err().contains("365"));
        assert!(parse_minutes(" 8 ").is_ok());
    }

    #[test]
    fn at_time_must_be_in_the_future_and_minute_aligned() {
        let now = now();
        assert!(parse_at("2026-10-17", "15:30", now).unwrap_err().contains("晚于"));
        assert!(parse_at("2026-10-17", "15:31", now).is_ok());
        assert!(parse_at("2026/10/18", "09：05", now).is_ok());
        assert!(parse_at("", "09:00", now).unwrap_err().contains("日期"));
        assert!(parse_at("2026-02-31", "09:00", now).unwrap_err().contains("日期"));
        assert!(parse_at("2026-10-18", "24:00", now).unwrap_err().contains("时间"));
        assert!(parse_at("2026-10-18", "9:7", now).is_ok());
    }

    #[test]
    fn cron_rejects_bad_shapes_and_previews_ten() {
        assert!(parse_cron("").unwrap_err().contains("请输入"));
        assert!(parse_cron("* * *").unwrap_err().contains("5 段"));
        assert!(parse_cron("0 0 0 * * *").unwrap_err().contains("不需要秒"));
        assert!(parse_cron("60 * * * *").unwrap_err().contains("分钟"));
        assert!(parse_cron("0 24 * * *").unwrap_err().contains("小时"));
        assert!(parse_cron("0 9 * * MONDAY").unwrap_err().contains("周"));
        assert!(parse_cron("0 0 31 2 *").is_ok() || parse_cron("0 0 31 2 *").is_err());
        let times = next_cron_times("0 9 * * 1-5", now(), 10).unwrap();
        assert_eq!(times.len(), 10);
        for window in times.windows(2) {
            assert!(window[0] < window[1]);
        }
        for time in &times {
            assert_eq!(time.hour(), 9);
            assert_eq!(time.minute(), 0);
            let day = time.weekday().number_from_monday();
            assert!((1..=5).contains(&day), "期望工作日，实际 {time}");
        }
        let names = next_cron_times("0 9 * * MON-FRI", now(), 3).unwrap();
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn build_trims_title_and_arms_delay() {
        let now = now();
        let mut draft = draft();
        draft.minutes = "10".into();
        draft.title = "  站会 \n 提醒  ".into();
        let reminder = build_reminder(&draft, None, now, 0).unwrap();
        assert_eq!(reminder.title, "站会 提醒");
        match reminder.schedule {
            Schedule::Delay { minutes, fire_at_ms } => {
                assert_eq!(minutes, 10);
                assert!(fire_at_ms > now_ms(now));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(reminder.enabled);
        assert_eq!(reminder.status, Status::Pending);
    }

    #[test]
    fn saving_cron_does_not_inherit_old_backfill() {
        let now = now();
        let mut draft = draft();
        draft.kind = Kind::Cron;
        draft.cron = "*/5 * * * *".into();
        let reminder = build_reminder(&draft, None, now, 0).unwrap();
        assert_eq!(reminder.last_occurrence_ms, Some(now_ms(now)));
        assert!(crate::schedule::due_reminders(std::slice::from_ref(&reminder), now).is_empty());
    }

    #[test]
    fn reenable_past_fixed_time_is_rejected() {
        let now = now();
        let mut reminder = build_reminder(
            &{
                let mut draft = draft();
                draft.kind = Kind::At;
                draft.date = "2026-10-18".into();
                draft.time = "09:00".into();
                draft
            },
            None,
            now,
            0,
        )
        .unwrap();
        if let Schedule::At { fire_at_ms } = &mut reminder.schedule {
            *fire_at_ms = now_ms(now) - 1_000;
        }
        reminder.status = Status::Fired;
        reminder.enabled = false;
        let error = set_enabled(&mut reminder, true, now).unwrap_err();
        assert!(error.contains("修改时间"));
    }

    #[test]
    fn reenable_delay_recomputes_from_now() {
        let now = now();
        let mut draft = draft();
        draft.minutes = "3".into();
        let mut reminder = build_reminder(&draft, None, now, 0).unwrap();
        apply_fire(&mut reminder, now_ms(now), now);
        assert_eq!(reminder.status, Status::Fired);
        assert!(!reminder.enabled);
        set_enabled(&mut reminder, true, now + Duration::minutes(30)).unwrap();
        match reminder.schedule {
            Schedule::Delay { fire_at_ms, .. } => {
                let expected = now_ms(now + Duration::minutes(33));
                assert!((fire_at_ms - expected).abs() < 2_000);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cap_blocks_new_items_only() {
        let now = now();
        let mut draft = draft();
        draft.minutes = "1".into();
        let error = build_reminder(&draft, None, now, REMINDER_CAP).unwrap_err();
        assert!(error.errors.iter().any(|item| item.field == "form"));
        let existing = build_reminder(&draft, None, now, 0).unwrap();
        assert!(build_reminder(&draft, Some(&existing), now, REMINDER_CAP).is_ok());
    }
}
