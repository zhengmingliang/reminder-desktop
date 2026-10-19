use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike};

use crate::model::{millis_to_local, now_ms, Reminder, Schedule, Status};
use crate::validate::parse_cron;
use croner::Cron;

/// 一次到点。`missed` 是被合并掉的更早次数，弹窗只展示最近这一次。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueHit {
    pub id: String,
    pub occurrence_ms: i64,
    pub missed: u32,
    pub late: bool,
    pub snooze: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListText {
    pub summary: String,
    pub when_text: String,
    pub badge: String,
    pub badge_kind: String,
    pub fired_at_text: String,
    pub scheduled_text: String,
    /// 相对时间（如「5 分钟后」），概览大字用；无下一次触发时为空。
    pub distance_text: String,
}

/// 把窗口外框放到某块显示器的正中，再按已打开弹窗的序号稍微错开。
/// 显示器坐标可以是负数（主屏右侧或左侧的副屏），窗口不会被推出该屏幕。
pub fn center_on_monitor(
    mon_x: i32,
    mon_y: i32,
    mon_w: u32,
    mon_h: u32,
    win_w: u32,
    win_h: u32,
    stack_index: u32,
) -> (i32, i32) {
    let win_w = win_w.max(1);
    let win_h = win_h.max(1);
    let base_x = mon_x + (mon_w as i32 - win_w as i32) / 2;
    let base_y = mon_y + (mon_h as i32 - win_h as i32) / 2;
    let shift = (stack_index as i32 % 6) * 28;
    let max_x = mon_x + mon_w as i32 - win_w as i32;
    let max_y = mon_y + mon_h as i32 - win_h as i32;
    let x = (base_x + shift).clamp(mon_x, max_x.max(mon_x));
    let y = (base_y + shift).clamp(mon_y, max_y.max(mon_y));
    (x, y)
}

pub fn due_reminders(reminders: &[Reminder], now: DateTime<Local>) -> Vec<DueHit> {
    let mut hits = Vec::new();
    for reminder in reminders {
        if let Some(hit) = due_one(reminder, now) {
            hits.push(hit);
        }
    }
    hits
}

fn due_one(reminder: &Reminder, now: DateTime<Local>) -> Option<DueHit> {
    if !reminder.enabled {
        return None;
    }
    let now_ms = now_ms(now);
    if let Some(until) = reminder.snooze_until_ms {
        if now_ms < until {
            return None;
        }
        return Some(DueHit {
            id: reminder.id.clone(),
            occurrence_ms: until,
            missed: 0,
            late: now_ms > until + 30_000,
            snooze: true,
        });
    }
    match &reminder.schedule {
        Schedule::Delay { fire_at_ms, .. } | Schedule::At { fire_at_ms } => {
            if reminder.status != Status::Pending || *fire_at_ms > now_ms {
                return None;
            }
            Some(DueHit {
                id: reminder.id.clone(),
                occurrence_ms: *fire_at_ms,
                missed: 0,
                late: now_ms > fire_at_ms + 30_000,
                snooze: false,
            })
        }
        Schedule::Cron { expression } => {
            if reminder.status == Status::Fired {
                return None;
            }
            let cron = parse_cron(expression).ok()?;
            let origin_ms = reminder
                .last_occurrence_ms
                .unwrap_or(reminder.created_at_ms);
            let (latest, missed) = latest_cron_hit(&cron, millis_to_local(origin_ms), now)?;
            let occurrence_ms = latest.timestamp_millis();
            Some(DueHit {
                id: reminder.id.clone(),
                occurrence_ms,
                missed,
                late: missed == 0 && now_ms > occurrence_ms + 30_000,
                snooze: false,
            })
        }
    }
}

/// 从 origin 之后找到不超过 now 的最近一次。超过 120 次就停止精确计数，避免每分钟表达式在长期离线后卡住。
fn latest_cron_hit(
    cron: &Cron,
    origin: DateTime<Local>,
    now: DateTime<Local>,
) -> Option<(DateTime<Local>, u32)> {
    let mut cursor = origin;
    let mut latest = None;
    let mut extra = 0u32;
    for _ in 0..120 {
        let Ok(next) = cron.find_next_occurrence(&cursor, false) else {
            break;
        };
        if next > now {
            break;
        }
        if latest.is_some() {
            extra = extra.saturating_add(1);
        }
        latest = Some(next);
        cursor = next;
    }
    let mut truncated = false;
    if let Some(found) = latest {
        if matches!(cron.find_next_occurrence(&found, false), Ok(more) if more <= now) {
            truncated = true;
            if let Some(tail) = binary_latest(cron, found, now) {
                latest = Some(tail);
            }
        }
    }
    let latest = latest.filter(|time| *time > origin)?;
    Some((latest, if truncated { 100 } else { extra }))
}

fn binary_latest(cron: &Cron, after: DateTime<Local>, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let mut lo = after;
    let mut hi = now;
    let mut best = None;
    for _ in 0..48 {
        if hi <= lo {
            break;
        }
        let mid = lo + (hi - lo) / 2;
        match cron.find_next_occurrence(&mid, true) {
            Ok(hit) if hit <= now && hit > after => {
                best = Some(hit);
                lo = hit + Duration::seconds(1);
            }
            _ => hi = mid,
        }
    }
    best
}

/// 下一次尚未处理的触发点，供调度线程决定要睡多久。
pub fn next_wake(reminders: &[Reminder], now: DateTime<Local>) -> Option<DateTime<Local>> {
    let mut best: Option<DateTime<Local>> = None;
    for reminder in reminders {
        if !reminder.enabled {
            continue;
        }
        if let Some(until) = reminder.snooze_until_ms {
            let time = millis_to_local(until);
            if time <= now {
                return Some(now);
            }
            best = Some(match best {
                Some(prev) if prev < time => prev,
                _ => time,
            });
            continue;
        }
        let candidate = match &reminder.schedule {
            Schedule::Delay { fire_at_ms, .. } | Schedule::At { fire_at_ms }
                if reminder.status == Status::Pending =>
            {
                Some(millis_to_local(*fire_at_ms))
            }
            Schedule::Cron { expression } if reminder.status != Status::Fired => {
                let Ok(cron) = parse_cron(expression) else {
                    continue;
                };
                let origin_ms = reminder
                    .last_occurrence_ms
                    .unwrap_or(reminder.created_at_ms);
                cron.find_next_occurrence(&millis_to_local(origin_ms), false).ok()
            }
            _ => None,
        };
        if let Some(time) = candidate {
            if time <= now {
                return Some(now);
            }
            best = Some(match best {
                Some(prev) if prev < time => prev,
                _ => time,
            });
        }
    }
    best
}

pub fn list_texts(reminder: &Reminder, now: DateTime<Local>) -> ListText {
    let (badge, badge_kind) = if reminder.snooze_until_ms.is_some() && reminder.enabled {
        ("稍后", "snooze")
    } else if reminder.status == Status::Fired {
        ("已触发", "fired")
    } else if !reminder.enabled {
        ("已暂停", "paused")
    } else if matches!(reminder.schedule, Schedule::Cron { .. }) {
        ("循环", "recurring")
    } else {
        ("待触发", "pending")
    };

    let summary = match &reminder.schedule {
        Schedule::Delay { minutes, .. } => format!("延迟 · {minutes} 分钟"),
        Schedule::At { fire_at_ms } => {
            format!("指定时间 · {}", format_point(millis_to_local(*fire_at_ms), now, false))
        }
        Schedule::Cron { expression } => {
            let shown = if expression.chars().count() > 28 {
                format!("{}…", expression.chars().take(28).collect::<String>())
            } else {
                expression.clone()
            };
            format!("Cron · {shown}")
        }
    };

    let when_text = if let Some(until) = reminder.snooze_until_ms.filter(|_| reminder.enabled) {
        format!("稍后 · {}", format_point(millis_to_local(until), now, true))
    } else if reminder.status == Status::Fired {
        match reminder.last_fired_at_ms {
            Some(ms) => format!("已于 {}", format_point(millis_to_local(ms), now, true)),
            None => "已触发".to_string(),
        }
    } else if !reminder.enabled {
        match &reminder.schedule {
            Schedule::Delay { .. } => "暂停 · 启用后从当前时间重新计时".to_string(),
            Schedule::At { fire_at_ms } => {
                format!("暂停 · {}", format_point(millis_to_local(*fire_at_ms), now, false))
            }
            Schedule::Cron { .. } => "暂停 · 启用后从下一周期继续".to_string(),
        }
    } else {
        match next_fire_display(reminder, now) {
            Some(text) => text,
            None => "等待下一周期".to_string(),
        }
    };

    let fired_at_text = reminder
        .last_fired_at_ms
        .map(|ms| format_point(millis_to_local(ms), now, false))
        .unwrap_or_default();
    let scheduled_text = reminder.snooze_until_ms
        .filter(|_| reminder.enabled)
        .map(|ms| format_point(millis_to_local(ms), now, true))
        .unwrap_or_else(|| match &reminder.schedule {
            Schedule::Delay { fire_at_ms, .. } | Schedule::At { fire_at_ms } => {
                format_point(millis_to_local(*fire_at_ms), now, false)
            }
            Schedule::Cron { .. } => String::new(),
        });
    // 相对时间：仅对已启用且确有下一次触发的提醒给出（概览「下一次提醒」大字）。
    let distance_text = if reminder.enabled && reminder.status != Status::Fired {
        let next_at: Option<DateTime<Local>> = if let Some(until) = reminder.snooze_until_ms {
            Some(millis_to_local(until))
        } else {
            match &reminder.schedule {
                Schedule::Delay { fire_at_ms, .. } | Schedule::At { fire_at_ms } => {
                    Some(millis_to_local(*fire_at_ms))
                }
                Schedule::Cron { expression } => {
                    let next = parse_cron(expression).ok().and_then(|cron| {
                        let origin_ms = reminder
                            .last_occurrence_ms
                            .unwrap_or(reminder.created_at_ms);
                        cron.find_next_occurrence(&millis_to_local(origin_ms), false).ok()
                    });
                    next
                }
            }
        };
        next_at
            .filter(|at| *at > now)
            .map(|at| format_distance(now, at))
            .unwrap_or_default()
    } else {
        String::new()
    };

    ListText {
        summary,
        when_text,
        badge: badge.to_string(),
        badge_kind: badge_kind.to_string(),
        fired_at_text,
        scheduled_text,
        distance_text,
    }
}

fn next_fire_display(reminder: &Reminder, now: DateTime<Local>) -> Option<String> {
    match &reminder.schedule {
        Schedule::Delay { fire_at_ms, .. } => {
            Some(format_point(millis_to_local(*fire_at_ms), now, true))
        }
        Schedule::At { fire_at_ms } => Some(format_point(millis_to_local(*fire_at_ms), now, false)),
        Schedule::Cron { expression } => {
            let cron = parse_cron(expression).ok()?;
            let origin_ms = reminder
                .last_occurrence_ms
                .unwrap_or(reminder.created_at_ms);
            let next = cron
                .find_next_occurrence(&millis_to_local(origin_ms), false)
                .ok()?;
            Some(format_point(next, now, false))
        }
    }
}

pub fn format_point(target: DateTime<Local>, now: DateTime<Local>, with_seconds: bool) -> String {
    let clock = if with_seconds && target.second() != 0 {
        target.format("%H:%M:%S").to_string()
    } else {
        target.format("%H:%M").to_string()
    };
    let today = now.date_naive();
    let day = target.date_naive();
    if day == today {
        format!("今天 {clock}")
    } else if day == today + Duration::days(1) {
        format!("明天 {clock}")
    } else if day == today - Duration::days(1) {
        format!("昨天 {clock}")
    } else if target.year() == now.year() {
        format!("{} {clock}", target.format("%m-%d"))
    } else {
        format!("{} {clock}", target.format("%Y-%m-%d"))
    }
}

pub fn format_preview_line<Tz: TimeZone>(target: DateTime<Tz>, now_year: i32) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let weekday = match target.weekday() {
        chrono::Weekday::Mon => "周一",
        chrono::Weekday::Tue => "周二",
        chrono::Weekday::Wed => "周三",
        chrono::Weekday::Thu => "周四",
        chrono::Weekday::Fri => "周五",
        chrono::Weekday::Sat => "周六",
        chrono::Weekday::Sun => "周日",
    };
    if target.year() == now_year {
        format!("{} {weekday}", target.format("%m-%d %H:%M"))
    } else {
        format!("{} {weekday}", target.format("%Y-%m-%d %H:%M"))
    }
}

pub fn format_distance(from: DateTime<Local>, to: DateTime<Local>) -> String {
    let secs = (to - from).num_seconds();
    if secs <= 0 {
        return "已到点".to_string();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days} 天"));
    }
    if hours > 0 {
        parts.push(format!("{hours} 小时"));
    }
    if minutes > 0 || parts.is_empty() {
        parts.push(format!("{minutes} 分钟"));
    }
    format!("{}后", parts.join(" "))
}

/// 概览「今天时间线」里的一行。kind：fired / snooze / missed / pending / now。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineEntry {
    pub at_ms: i64,
    pub time_text: String,
    pub title: String,
    pub status: String,
    pub kind: String,
    pub is_next: bool,
}

/// 汇总今天 0 点到 24 点之间所有会触发的时点（含已触发 / 已错过 / 已稍后），
/// 过去的排在前面，未来的排在后面，第一个未来项标记为 is_next。
pub fn today_timeline(reminders: &[Reminder], now: DateTime<Local>) -> Vec<TimelineEntry> {
    let start = local_day_start(now);
    let end = start + Duration::days(1);
    let start_ms = local_to_millis(start);
    let end_ms = local_to_millis(end);
    let now_ms = local_to_millis(now);

    let mut entries: Vec<TimelineEntry> = Vec::new();
    for reminder in reminders {
        let mut push = |at_ms: i64, kind: &str, status: &str| {
            if at_ms < start_ms || at_ms >= end_ms {
                return;
            }
            let time_text = millis_to_local(at_ms).format("%H:%M").to_string();
            entries.push(TimelineEntry {
                at_ms,
                time_text,
                title: reminder.title.clone(),
                status: status.to_string(),
                kind: kind.to_string(),
                is_next: false,
            });
        };
        // 已被推迟到今天的触发点。
        if reminder.enabled {
            if let Some(until) = reminder.snooze_until_ms {
                push(until, "snooze", "已稍后");
            }
        }
        match &reminder.schedule {
            Schedule::Delay { fire_at_ms, .. } | Schedule::At { fire_at_ms } => {
                let fired = reminder
                    .last_fired_at_ms
                    .map(|fired| fired >= *fire_at_ms)
                    .unwrap_or(false);
                let (kind, status) = if fired {
                    ("fired", "已触发")
                } else if *fire_at_ms <= now_ms {
                    ("missed", "已错过")
                } else {
                    ("pending", "待触发")
                };
                push(*fire_at_ms, kind, status);
            }
            Schedule::Cron { expression } => {
                let Ok(cron) = parse_cron(expression) else {
                    continue;
                };
                let mut cursor = if reminder.created_at_ms > start_ms {
                    millis_to_local(reminder.created_at_ms)
                } else {
                    start
                };
                for _ in 0..64 {
                    let Ok(next) = cron.find_next_occurrence(&cursor, false) else {
                        break;
                    };
                    if next >= end {
                        break;
                    }
                    let at_ms = local_to_millis(next);
                    let fired = reminder
                        .last_fired_at_ms
                        .map(|fired| fired >= at_ms)
                        .unwrap_or(false);
                    let (kind, status) = if fired {
                        ("fired", "已触发")
                    } else if at_ms <= now_ms {
                        ("missed", "已错过")
                    } else {
                        ("pending", "待触发")
                    };
                    push(at_ms, kind, status);
                    cursor = next + Duration::seconds(1);
                }
            }
        }
    }
    entries.sort_by_key(|entry| entry.at_ms);

    // 第一个未来项就是「下一次提醒」，展示相对时间。
    if let Some(next) = entries.iter_mut().find(|entry| entry.at_ms > now_ms) {
        next.is_next = true;
        next.status = format_distance(now, millis_to_local(next.at_ms));
    }

    // 条目太多时做窗口截断：过去保留最近 4 条，未来保留最近 8 条。
    if entries.len() > 12 {
        let past_end = entries
            .iter()
            .position(|entry| entry.at_ms > now_ms)
            .unwrap_or(entries.len());
        let (past, future) = entries.split_at(past_end);
        let past_from = past.len().saturating_sub(4);
        let future_to = future.len().min(8);
        let mut kept: Vec<TimelineEntry> = past[past_from..].to_vec();
        kept.extend_from_slice(&future[..future_to]);
        entries = kept;
    }

    // 在「现在」的位置插入分割线：过去与未来都存在时才有意义。
    if entries.iter().any(|entry| entry.at_ms > now_ms) {
        let at = entries.iter().position(|entry| entry.at_ms > now_ms).unwrap_or(entries.len());
        entries.insert(
            at,
            TimelineEntry {
                at_ms: now_ms,
                time_text: now.format("%H:%M").to_string(),
                title: String::new(),
                status: "现在".to_string(),
                kind: "now".to_string(),
                is_next: false,
            },
        );
    }
    entries
}

/// 概览「最近记录」里的一行。kind：ok / warn / danger。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordEntry {
    pub time_text: String,
    pub title: String,
    pub status: String,
    pub kind: String,
}

/// 用每条提醒的最近一次触发时间拼出最近记录，最多 4 条。
pub fn recent_records(reminders: &[Reminder], now: DateTime<Local>) -> Vec<RecordEntry> {
    let mut fired: Vec<RecordEntry> = Vec::new();
    for reminder in reminders {
        let Some(ms) = reminder.last_fired_at_ms else {
            continue;
        };
        let (status, kind) = if reminder.snooze_count > 0 {
            ("已稍后", "warn")
        } else {
            ("已确认", "ok")
        };
        fired.push(RecordEntry {
            time_text: format_point(millis_to_local(ms), now, false),
            title: reminder.title.clone(),
            status: status.to_string(),
            kind: kind.to_string(),
        });
    }
    fired.sort_by(|left, right| left.time_text.cmp(&right.time_text));
    fired.reverse();
    fired.truncate(4);
    fired
}

fn local_day_start(now: DateTime<Local>) -> DateTime<Local> {
    let naive = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| now.naive_local());
    Local
        .from_local_datetime(&naive)
        .single()
        .unwrap_or(now)
}

fn local_to_millis(time: DateTime<Local>) -> i64 {
    time.timestamp_millis()
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Schedule, Status};
    use chrono::TimeZone;

    fn fixed(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, mi, s).single().expect("valid")
    }

    fn sample(schedule: Schedule, enabled: bool, status: Status) -> Reminder {
        Reminder {
            id: "a".into(),
            title: "t".into(),
            body: String::new(),
            schedule,
            sound_enabled: true,
            sound_path: None,
            enabled,
            status,
            created_at_ms: 0,
            updated_at_ms: 0,
            last_fired_at_ms: None,
            last_occurrence_ms: None,
            snooze_until_ms: None,
            snooze_count: 0,
        }
    }

    #[test]
    fn centers_primary_and_negative_monitor() {
        assert_eq!(center_on_monitor(0, 0, 1920, 1080, 400, 300, 0), (760, 390));
        assert_eq!(
            center_on_monitor(-1920, 0, 1920, 1080, 400, 300, 0),
            (-1160, 390)
        );
        let (x, y) = center_on_monitor(0, 0, 800, 600, 400, 300, 5);
        assert!(x >= 0 && y >= 0);
        assert!(x + 400 <= 800);
        assert!(y + 300 <= 600);
    }

    #[test]
    fn window_larger_than_monitor_stays_at_origin() {
        assert_eq!(center_on_monitor(10, 20, 100, 80, 400, 300, 3), (10, 20));
    }

    #[test]
    fn one_shot_due_only_when_armed_and_time_reached() {
        let now = fixed(2026, 10, 17, 12, 0, 0);
        let past = now_ms(now - Duration::minutes(1));
        let future = now_ms(now + Duration::minutes(5));
        let due = sample(Schedule::At { fire_at_ms: past }, true, Status::Pending);
        assert!(due_reminders(&[due], now)[0].late);
        let waiting = sample(Schedule::At { fire_at_ms: future }, true, Status::Pending);
        assert!(due_reminders(&[waiting], now).is_empty());
        let paused = sample(Schedule::Delay { minutes: 5, fire_at_ms: past }, false, Status::Pending);
        assert!(due_reminders(&[paused], now).is_empty());
        let fired = sample(Schedule::At { fire_at_ms: past }, true, Status::Fired);
        assert!(due_reminders(&[fired], now).is_empty());
    }

    #[test]
    fn cron_fires_once_and_collapses_missed_slots() {
        let now = fixed(2026, 10, 17, 12, 0, 10);
        let mut reminder = sample(
            Schedule::Cron { expression: "0 * * * *".into() },
            true,
            Status::Recurring,
        );
        reminder.created_at_ms = now_ms(fixed(2026, 10, 17, 8, 0, 0));
        reminder.last_occurrence_ms = Some(reminder.created_at_ms);
        let hits = due_reminders(&[reminder.clone()], now);
        assert_eq!(hits.len(), 1);
        let shown = millis_to_local(hits[0].occurrence_ms);
        assert_eq!(shown.hour(), 12);
        assert_eq!(shown.minute(), 0);
        assert!(hits[0].missed >= 1);

        reminder.last_occurrence_ms = Some(hits[0].occurrence_ms);
        assert!(due_reminders(&[reminder], now).is_empty());
    }

    #[test]
    fn fresh_cron_does_not_backfill_before_save() {
        let now = fixed(2026, 10, 17, 12, 30, 0);
        let mut reminder = sample(
            Schedule::Cron { expression: "* * * * *".into() },
            true,
            Status::Recurring,
        );
        reminder.created_at_ms = now_ms(now);
        reminder.last_occurrence_ms = Some(now_ms(now));
        assert!(due_reminders(&[reminder], now).is_empty());
    }

    #[test]
    fn snooze_waits_then_fires_a_one_shot_again() {
        let now = fixed(2026, 10, 17, 12, 0, 0);
        let mut reminder = sample(
            Schedule::At { fire_at_ms: now_ms(now - Duration::hours(1)) },
            false,
            Status::Fired,
        );
        crate::validate::arm_snooze(&mut reminder, 15, now).unwrap();
        assert!(due_reminders(&[reminder.clone()], now).is_empty());
        assert!(crate::validate::arm_snooze(&mut reminder.clone(), 7, now).is_err());
        let later = now + Duration::minutes(15);
        let hits = due_reminders(&[reminder.clone()], later);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snooze);
        crate::validate::finish_snooze(&mut reminder, later);
        assert_eq!(reminder.status, Status::Fired);
        assert!(!reminder.enabled);
        assert!(reminder.snooze_until_ms.is_none());
        assert!(due_reminders(&[reminder], later).is_empty());
    }

    #[test]
    fn snoozed_cron_does_not_dump_missed_slots() {
        let now = fixed(2026, 10, 17, 12, 0, 0);
        let mut reminder = sample(
            Schedule::Cron { expression: "0 * * * *".into() },
            true,
            Status::Recurring,
        );
        reminder.created_at_ms = now_ms(now - Duration::hours(5));
        reminder.last_occurrence_ms = Some(reminder.created_at_ms);
        crate::validate::arm_snooze(&mut reminder, 30, now).unwrap();
        assert!(due_reminders(std::slice::from_ref(&reminder), now).is_empty());
        let later = now + Duration::minutes(30);
        let hits = due_reminders(std::slice::from_ref(&reminder), later);
        assert!(hits[0].snooze);
        crate::validate::finish_snooze(&mut reminder, later);
        assert!(reminder.enabled);
        assert_eq!(reminder.status, Status::Recurring);
        assert!(due_reminders(std::slice::from_ref(&reminder), later).is_empty());
    }
}
