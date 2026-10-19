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

/// 一次提前提示。`occurrence_ms` 是真正的触发点，`early_minutes` 是提前了多少分钟。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EarlyHit {
    pub id: String,
    pub occurrence_ms: i64,
    pub early_minutes: u32,
}

/// 一次被跳过的触发点。到点时不弹窗，只把计划推进过这一点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedHit {
    pub id: String,
    pub occurrence_ms: i64,
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

/// Cron 是否已结束：超过结束日期，或已触发次数达到上限。
fn cron_finished(reminder: &Reminder, end_date_ms: Option<i64>, max_occurrences: Option<u32>) -> bool {
    if let Some(max) = max_occurrences {
        if reminder.occurrence_count >= max {
            return true;
        }
    }
    if let Some(end_ms) = end_date_ms {
        if let Some(next_ms) = next_cron_ms(reminder) {
            if next_ms > end_ms {
                return true;
            }
        }
    }
    false
}

/// Cron 下一次触发点（毫秒）。
fn next_cron_ms(reminder: &Reminder) -> Option<i64> {
    let Schedule::Cron { expression, .. } = &reminder.schedule else {
        return None;
    };
    let origin_ms = reminder
        .last_occurrence_ms
        .unwrap_or(reminder.created_at_ms);
    next_cron_after(expression, origin_ms)
}

/// 从 origin 之后找 expression 的下一次触发点。
fn next_cron_after(expression: &str, origin_ms: i64) -> Option<i64> {
    let cron = parse_cron(expression).ok()?;
    cron.find_next_occurrence(&millis_to_local(origin_ms), false)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// 下一次触发点（毫秒）。暂停、稍后期间或已完成的一次性提醒返回 None。
pub fn next_occurrence_ms(reminder: &Reminder) -> Option<i64> {
    if reminder.is_deleted() {
        return None;
    }
    if !reminder.enabled || reminder.snooze_until_ms.is_some() {
        return None;
    }
    match &reminder.schedule {
        Schedule::Delay { fire_at_ms, .. } | Schedule::At { fire_at_ms }
            if reminder.status == Status::Pending =>
        {
            Some(*fire_at_ms)
        }
        Schedule::Cron {
            expression,
            end_date_ms,
            max_occurrences,
        } if reminder.status != Status::Fired => {
            if cron_finished(reminder, *end_date_ms, *max_occurrences) {
                return None;
            }
            let next = next_cron_ms(reminder)?;
            // 被跳过的一次不算「下一次」，预告它后面的那次。
            if reminder.skip_occurrence_ms == Some(next) {
                let after = next_cron_after(expression, next)?;
                if let Some(end_ms) = end_date_ms {
                    if after > *end_ms {
                        return None;
                    }
                }
                return Some(after);
            }
            Some(next)
        }
        _ => None,
    }
}

/// 到点前该弹「提前提醒」的条目。每个触发点只弹一次，真实触发不受影响。
pub fn early_reminders(reminders: &[Reminder], now: DateTime<Local>) -> Vec<EarlyHit> {
    let mut hits = Vec::new();
    let now_ms = now_ms(now);
    for reminder in reminders {
        if reminder.early_minutes == 0 {
            continue;
        }
        let Some(occurrence_ms) = next_occurrence_ms(reminder) else {
            continue;
        };
        let early_ms = occurrence_ms - i64::from(reminder.early_minutes) * 60_000;
        if early_ms > now_ms || occurrence_ms <= now_ms {
            continue;
        }
        if reminder.early_notice_sent_ms == Some(occurrence_ms) {
            continue;
        }
        hits.push(EarlyHit {
            id: reminder.id.clone(),
            occurrence_ms,
            early_minutes: reminder.early_minutes,
        });
    }
    hits
}

/// 到点该静默跳过的条目：把计划推进过被跳过的触发点，不弹窗。
pub fn skipped_reminders(reminders: &[Reminder], now: DateTime<Local>) -> Vec<SkippedHit> {
    let mut hits = Vec::new();
    let now_ms = now_ms(now);
    for reminder in reminders {
        let Some(skip_ms) = reminder.skip_occurrence_ms else {
            continue;
        };
        if reminder.is_deleted() {
            continue;
        }
        if skip_ms > now_ms || !reminder.enabled || reminder.snooze_until_ms.is_some() {
            continue;
        }
        if !matches!(reminder.schedule, Schedule::Cron { .. }) || reminder.status == Status::Fired {
            continue;
        }
        let origin_ms = reminder
            .last_occurrence_ms
            .unwrap_or(reminder.created_at_ms);
        // 只推进还没过去的跳过点；已经被更晚触发点盖过的不用再动。
        if skip_ms > origin_ms {
            hits.push(SkippedHit {
                id: reminder.id.clone(),
                occurrence_ms: skip_ms,
            });
        }
    }
    hits
}

fn due_one(reminder: &Reminder, now: DateTime<Local>) -> Option<DueHit> {
    if reminder.is_deleted() {
        return None;
    }
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
        Schedule::Cron {
            expression,
            end_date_ms,
            max_occurrences,
        } => {
            if reminder.status == Status::Fired {
                return None;
            }
            if let Some(max) = max_occurrences {
                if reminder.occurrence_count >= *max {
                    return None;
                }
            }
            let cron = parse_cron(expression).ok()?;
            let origin_ms = reminder
                .last_occurrence_ms
                .unwrap_or(reminder.created_at_ms);
            let (latest, missed) = latest_cron_hit(&cron, millis_to_local(origin_ms), now)?;
            let occurrence_ms = latest.timestamp_millis();
            // 被跳过的触发点不弹窗，由 skipped_reminders 单独推进。
            if reminder.skip_occurrence_ms == Some(occurrence_ms) {
                return None;
            }
            // 结束日期之后的触发点不再弹。
            if let Some(end_ms) = end_date_ms {
                if occurrence_ms > *end_ms {
                    return None;
                }
            }
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
        // 已移入回收站的条目不再参与调度；漏掉会让调度线程带着锁忙转。
        if reminder.is_deleted() {
            continue;
        }
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
            Schedule::Cron { .. } if reminder.status != Status::Fired => {
                next_occurrence_ms(reminder).map(millis_to_local)
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
            // 设了提前提醒时，提前点也要唤醒一次，否则会睡到真实触发点才弹。
            if reminder.early_minutes > 0
                && reminder.early_notice_sent_ms != Some(time.timestamp_millis())
            {
                let early = time - Duration::minutes(i64::from(reminder.early_minutes));
                if early <= now {
                    return Some(now);
                }
                best = Some(match best {
                    Some(prev) if prev < early => prev,
                    _ => early,
                });
            }
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
        Schedule::Cron { expression, .. } => {
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
                Schedule::Cron { .. } => {
                    next_occurrence_ms(reminder).map(millis_to_local)
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
        Schedule::Cron { .. } => {
            let next = next_occurrence_ms(reminder)?;
            Some(format_point(millis_to_local(next), now, false))
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
    // 同一个表达式往往被多条提醒共用，按表达式缓存解析结果，避免每条都重新 parse_cron。
    let mut cron_cache: std::collections::HashMap<&str, Cron> = std::collections::HashMap::new();
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
            Schedule::Cron { expression, .. } => {
                if !cron_cache.contains_key(expression.as_str()) {
                    match parse_cron(expression) {
                        Ok(parsed) => {
                            cron_cache.insert(expression.as_str(), parsed);
                        }
                        Err(_) => continue,
                    }
                }
                let cron = &cron_cache[expression.as_str()];
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
    use crate::model::{REMINDER_CAP, Schedule, Status};
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
            early_minutes: 0,
            early_notice_sent_ms: None,
            occurrence_count: 0,
            skip_occurrence_ms: None,
            deleted_at_ms: None,
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
            Schedule::Cron { expression: "0 * * * *".into(), end_date_ms: None, max_occurrences: None },
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
            Schedule::Cron { expression: "* * * * *".into(), end_date_ms: None, max_occurrences: None },
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
            Schedule::Cron { expression: "0 * * * *".into(), end_date_ms: None, max_occurrences: None },
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

    #[test]
    fn sleep_wake_backfills_a_much_later_one_shot_and_rechecks_immediately() {
        let scheduled = fixed(2026, 10, 17, 9, 0, 0);
        let wake = fixed(2026, 10, 17, 18, 30, 0); // 电脑睡了 9.5 小时
        let reminder = sample(
            Schedule::At { fire_at_ms: now_ms(scheduled) },
            true,
            Status::Pending,
        );
        let hits = due_reminders(std::slice::from_ref(&reminder), wake);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].late, "唤醒时已过点，应标记为错过");
        assert_eq!(hits[0].occurrence_ms, now_ms(scheduled), "补发仍显示原定时刻");
        // 调度线程不该再长睡：过期的提醒应让下一次唤醒立刻返回 now。
        assert_eq!(next_wake(std::slice::from_ref(&reminder), wake), Some(wake));
    }

    #[test]
    fn offline_minute_cron_collapses_to_latest_and_caps_the_count() {
        let origin = fixed(2026, 10, 10, 9, 0, 0);
        let now = origin + Duration::days(5);
        let mut reminder = sample(
            Schedule::Cron { expression: "* * * * *".into(), end_date_ms: None, max_occurrences: None },
            true,
            Status::Recurring,
        );
        reminder.created_at_ms = now_ms(origin);
        reminder.last_occurrence_ms = Some(now_ms(origin));
        let hits = due_reminders(std::slice::from_ref(&reminder), now);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].missed, 100, "长期离线后错过次数封顶，不逐条补发");
        let at = millis_to_local(hits[0].occurrence_ms);
        assert!(at <= now && now - at < Duration::seconds(60), "只取最近一次");
    }

    #[test]
    fn clock_jump_backward_does_not_fire_a_pending_one_shot_early() {
        let armed = fixed(2026, 10, 17, 12, 0, 0);
        let reminder = sample(
            Schedule::At { fire_at_ms: now_ms(armed) },
            true,
            Status::Pending,
        );
        let earlier = armed - Duration::hours(1); // 手动把时间往回调了一小时
        assert!(due_reminders(std::slice::from_ref(&reminder), earlier).is_empty());
        assert_eq!(
            next_wake(std::slice::from_ref(&reminder), earlier),
            Some(armed),
            "回拨后应等到原定时刻，而不是立刻触发"
        );
        assert_eq!(due_reminders(std::slice::from_ref(&reminder), armed).len(), 1);
    }

    #[test]
    fn clock_jump_forward_marks_the_one_shot_late() {
        let armed = fixed(2026, 10, 17, 12, 0, 0);
        let reminder = sample(
            Schedule::At { fire_at_ms: now_ms(armed) },
            true,
            Status::Pending,
        );
        let jumped = armed + Duration::hours(2); // 手动把时间调快了两小时
        let hits = due_reminders(std::slice::from_ref(&reminder), jumped);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].late);
    }

    #[test]
    fn early_window_fires_once_and_the_real_fire_still_happens() {
        let fire = fixed(2026, 10, 17, 12, 0, 0);
        let mut reminder = sample(
            Schedule::At { fire_at_ms: now_ms(fire) },
            true,
            Status::Pending,
        );
        reminder.early_minutes = 10;
        // 提前窗口（11:50）之前不弹。
        assert!(early_reminders(std::slice::from_ref(&reminder), fire - Duration::minutes(11)).is_empty());
        // 进入窗口弹一次。
        let hits = early_reminders(std::slice::from_ref(&reminder), fire - Duration::minutes(10));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].occurrence_ms, now_ms(fire));
        assert_eq!(hits[0].early_minutes, 10);
        // 窗口内不触发真实提醒。
        assert!(due_reminders(std::slice::from_ref(&reminder), fire - Duration::minutes(10)).is_empty());
        // 标记后同一触发点不再弹。
        crate::validate::mark_early_sent(&mut reminder, now_ms(fire), fire - Duration::minutes(10));
        assert!(early_reminders(std::slice::from_ref(&reminder), fire - Duration::minutes(9)).is_empty());
        // 到点后提前提醒不再补，真实提醒照常。
        assert!(early_reminders(std::slice::from_ref(&reminder), fire).is_empty());
        assert_eq!(due_reminders(std::slice::from_ref(&reminder), fire).len(), 1);
    }

    #[test]
    fn early_skips_zero_minutes_paused_fired_and_snoozed() {
        let fire = fixed(2026, 10, 17, 12, 0, 0);
        let in_window = fire - Duration::minutes(5);
        let zero = sample(Schedule::At { fire_at_ms: now_ms(fire) }, true, Status::Pending);
        assert!(early_reminders(std::slice::from_ref(&zero), in_window).is_empty());
        let mut paused = sample(Schedule::At { fire_at_ms: now_ms(fire) }, false, Status::Pending);
        paused.early_minutes = 10;
        assert!(early_reminders(std::slice::from_ref(&paused), in_window).is_empty());
        let mut fired = sample(Schedule::At { fire_at_ms: now_ms(fire) }, true, Status::Fired);
        fired.early_minutes = 10;
        assert!(early_reminders(std::slice::from_ref(&fired), in_window).is_empty());
        let mut snoozed = sample(Schedule::At { fire_at_ms: now_ms(fire) }, true, Status::Pending);
        snoozed.early_minutes = 10;
        snoozed.snooze_until_ms = Some(now_ms(fire + Duration::minutes(5)));
        assert!(early_reminders(std::slice::from_ref(&snoozed), in_window).is_empty());
    }

    #[test]
    fn early_uses_the_next_cron_occurrence() {
        let origin = fixed(2026, 10, 17, 11, 0, 0);
        let mut reminder = sample(
            Schedule::Cron { expression: "0 * * * *".into(), end_date_ms: None, max_occurrences: None },
            true,
            Status::Recurring,
        );
        reminder.created_at_ms = now_ms(origin);
        reminder.last_occurrence_ms = Some(now_ms(origin));
        reminder.early_minutes = 10;
        // 下一个整点是 12:00，提前窗口从 11:50 开始。
        assert!(early_reminders(std::slice::from_ref(&reminder), fixed(2026, 10, 17, 11, 49, 0)).is_empty());
        let hits = early_reminders(std::slice::from_ref(&reminder), fixed(2026, 10, 17, 11, 50, 0));
        assert_eq!(hits.len(), 1);
        let occurrence = millis_to_local(hits[0].occurrence_ms);
        assert_eq!((occurrence.hour(), occurrence.minute()), (12, 0));
        crate::validate::mark_early_sent(&mut reminder, hits[0].occurrence_ms, origin);
        assert!(early_reminders(std::slice::from_ref(&reminder), fixed(2026, 10, 17, 11, 55, 0)).is_empty());
    }

    #[test]
    fn cron_stops_after_end_date_and_max_occurrences() {
        let origin = fixed(2026, 10, 17, 8, 0, 0);
        let mut reminder = sample(
            Schedule::Cron {
                expression: "0 * * * *".into(),
                end_date_ms: Some(now_ms(fixed(2026, 10, 17, 12, 0, 0))),
                max_occurrences: None,
            },
            true,
            Status::Recurring,
        );
        reminder.created_at_ms = now_ms(origin);
        reminder.last_occurrence_ms = Some(now_ms(origin));
        // 11:00 的触发点在结束日期前，正常弹。
        let hits = due_reminders(std::slice::from_ref(&reminder), fixed(2026, 10, 17, 11, 0, 10));
        assert_eq!(hits.len(), 1);
        // 13:00 再看：最近一次触发点 13:00 已过结束时间 12:00，不弹。
        assert!(due_reminders(std::slice::from_ref(&reminder), fixed(2026, 10, 17, 13, 0, 10)).is_empty());
        // 把已处理的触发点推进到 13:00 后，next_occurrence 应返回 None（下一个 14:00 超过结束日期）。
        reminder.last_occurrence_ms = Some(now_ms(fixed(2026, 10, 17, 13, 0, 0)));
        assert_eq!(next_occurrence_ms(&reminder), None, "下一个整点 14:00 超过结束日期");

        // 次数限制：已触发 3 次、上限 3 次后不再弹。
        let mut capped = sample(
            Schedule::Cron {
                expression: "0 * * * *".into(),
                end_date_ms: None,
                max_occurrences: Some(3),
            },
            true,
            Status::Recurring,
        );
        capped.created_at_ms = now_ms(origin);
        capped.last_occurrence_ms = Some(now_ms(origin));
        capped.occurrence_count = 2;
        let later = fixed(2026, 10, 17, 9, 0, 10);
        assert_eq!(due_reminders(std::slice::from_ref(&capped), later).len(), 1);
        crate::validate::apply_fire(&mut capped, now_ms(fixed(2026, 10, 17, 9, 0, 0)), later);
        assert_eq!(capped.occurrence_count, 3);
        assert!(due_reminders(std::slice::from_ref(&capped), fixed(2026, 10, 17, 10, 0, 10)).is_empty());
        assert_eq!(next_occurrence_ms(&capped), None);
    }

    #[test]
    fn next_wake_also_wakes_at_the_early_point() {
        let fire = fixed(2026, 10, 17, 12, 0, 0);
        let mut reminder = sample(
            Schedule::At { fire_at_ms: now_ms(fire) },
            true,
            Status::Pending,
        );
        reminder.early_minutes = 10;
        let early = fire - Duration::minutes(10);
        assert_eq!(
            next_wake(std::slice::from_ref(&reminder), fire - Duration::minutes(30)),
            Some(early),
            "应先睡到提前点"
        );
        // 提前提醒已发出后，下一次唤醒回到真实触发点。
        reminder.early_notice_sent_ms = Some(now_ms(fire));
        assert_eq!(
            next_wake(std::slice::from_ref(&reminder), fire - Duration::minutes(9)),
            Some(fire)
        );
        // 提前点已过但还没发过：立刻唤醒去补弹。
        reminder.early_notice_sent_ms = None;
        assert_eq!(
            next_wake(std::slice::from_ref(&reminder), fire - Duration::minutes(8)),
            Some(fire - Duration::minutes(8))
        );
    }

    /// 移入回收站后，调度相关的所有入口都要当它不存在：
    /// 不弹、不提前、不推进跳过点，next_wake 也不能因残留的稍后时间忙转。
    #[test]
    fn deleted_reminders_are_invisible_to_scheduling() {
        let now = fixed(2026, 10, 17, 12, 0, 0);
        let past = now_ms(now - Duration::minutes(1));

        let mut due = sample(Schedule::At { fire_at_ms: past }, true, Status::Pending);
        due.deleted_at_ms = Some(past);
        assert!(due_reminders(std::slice::from_ref(&due), now).is_empty());

        let mut snoozed = sample(Schedule::At { fire_at_ms: past }, true, Status::Pending);
        snoozed.snooze_until_ms = Some(past);
        snoozed.deleted_at_ms = Some(past);
        // 稍后时间已过：不门控的话 due 会弹、next_wake 会立刻唤醒导致忙转。
        assert!(due_reminders(std::slice::from_ref(&snoozed), now).is_empty());
        assert_eq!(next_wake(std::slice::from_ref(&snoozed), now), None);

        let mut early = sample(
            Schedule::Cron { expression: "0 * * * *".into(), end_date_ms: None, max_occurrences: None },
            true,
            Status::Recurring,
        );
        early.early_minutes = 5;
        early.created_at_ms = now_ms(fixed(2026, 10, 17, 8, 0, 0));
        early.last_occurrence_ms = Some(early.created_at_ms);
        early.deleted_at_ms = Some(past);
        assert!(early_reminders(std::slice::from_ref(&early), now).is_empty());

        let mut skipped = sample(
            Schedule::Cron { expression: "0 * * * *".into(), end_date_ms: None, max_occurrences: None },
            true,
            Status::Recurring,
        );
        skipped.created_at_ms = now_ms(fixed(2026, 10, 17, 8, 0, 0));
        skipped.last_occurrence_ms = Some(skipped.created_at_ms);
        skipped.skip_occurrence_ms = Some(past);
        skipped.deleted_at_ms = Some(past);
        assert!(skipped_reminders(std::slice::from_ref(&skipped), now).is_empty());

        let mut waiting = sample(
            Schedule::At { fire_at_ms: now_ms(now + Duration::minutes(5)) },
            true,
            Status::Pending,
        );
        waiting.deleted_at_ms = Some(past);
        assert_eq!(next_wake(std::slice::from_ref(&waiting), now), None);
        assert_eq!(next_occurrence_ms(&waiting), None);
    }

    #[test]
    fn skip_occurrence_is_not_due_but_gets_advanced() {
        let origin = fixed(2026, 10, 17, 8, 0, 0);
        let mut reminder = sample(
            Schedule::Cron {
                expression: "0 * * * *".into(),
                end_date_ms: None,
                max_occurrences: None,
            },
            true,
            Status::Recurring,
        );
        reminder.created_at_ms = now_ms(origin);
        reminder.last_occurrence_ms = Some(now_ms(origin));
        // 用户跳过 9:00 这次。
        reminder.skip_occurrence_ms = Some(now_ms(fixed(2026, 10, 17, 9, 0, 0)));

        // 9:00 到了：不弹，但出现在 skipped_reminders 里。
        let now = fixed(2026, 10, 17, 9, 0, 10);
        assert!(due_reminders(std::slice::from_ref(&reminder), now).is_empty());
        let skips = skipped_reminders(std::slice::from_ref(&reminder), now);
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].occurrence_ms, now_ms(fixed(2026, 10, 17, 9, 0, 0)));

        // 推进后 10:00 正常弹。
        crate::validate::apply_skip(&mut reminder, skips[0].occurrence_ms, now);
        assert_eq!(reminder.skip_occurrence_ms, None);
        assert!(skipped_reminders(std::slice::from_ref(&reminder), now).is_empty());
        let hits = due_reminders(std::slice::from_ref(&reminder), fixed(2026, 10, 17, 10, 0, 10));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].occurrence_ms, now_ms(fixed(2026, 10, 17, 10, 0, 0)));
    }

    #[test]
    fn next_occurrence_hops_over_the_skipped_point() {
        let origin = fixed(2026, 10, 17, 8, 0, 0);
        let mut reminder = sample(
            Schedule::Cron {
                expression: "0 * * * *".into(),
                end_date_ms: None,
                max_occurrences: None,
            },
            true,
            Status::Recurring,
        );
        reminder.created_at_ms = now_ms(origin);
        reminder.last_occurrence_ms = Some(now_ms(origin));
        assert_eq!(
            next_occurrence_ms(&reminder),
            Some(now_ms(fixed(2026, 10, 17, 9, 0, 0)))
        );
        // 跳过 9:00 后，预告的「下一次」变成 10:00。
        reminder.skip_occurrence_ms = Some(now_ms(fixed(2026, 10, 17, 9, 0, 0)));
        assert_eq!(
            next_occurrence_ms(&reminder),
            Some(now_ms(fixed(2026, 10, 17, 10, 0, 0)))
        );
        // 提前提醒也以下一次（10:00）为准，不会在 9:00 前弹。
        reminder.early_minutes = 5;
        let early_window = fixed(2026, 10, 17, 8, 56, 0);
        assert!(early_reminders(std::slice::from_ref(&reminder), early_window).is_empty());
        let hits = early_reminders(
            std::slice::from_ref(&reminder),
            fixed(2026, 10, 17, 9, 56, 0),
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].occurrence_ms, now_ms(fixed(2026, 10, 17, 10, 0, 0)));
    }

    #[test]
    fn delay_stores_absolute_time_so_a_timezone_shift_does_not_move_it() {
        let at = fixed(2026, 10, 17, 12, 0, 0);
        let reminder = sample(
            Schedule::Delay { minutes: 5, fire_at_ms: now_ms(at) },
            true,
            Status::Pending,
        );
        // 换时区或夏令时只改本地显示，不改毫秒时间戳，触发点保持不变。
        assert!(due_reminders(std::slice::from_ref(&reminder), at - Duration::seconds(1)).is_empty());
        let hits = due_reminders(std::slice::from_ref(&reminder), at);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].occurrence_ms, now_ms(at));
        assert!(!hits[0].late);
    }

    /// 大量提醒时，列表与 Cron 预览的计算量实测（不凭感觉判断）。
    /// 用 `--nocapture` 可以看到具体耗时；线程在 debug 下也用宽松预算兜底。
    #[test]
    fn large_list_compute_stays_within_budget() {
        use std::time::Instant;
        let now = fixed(2026, 10, 17, 12, 0, 0);
        let total = REMINDER_CAP;
        let mut reminders: Vec<Reminder> = Vec::with_capacity(total);
        for i in 0..total {
            // 贴近真实使用：多数是一次性提醒，少量每小时 Cron，极少数每分钟 Cron。
            let mut reminder = match i % 10 {
                0 => sample(
                    Schedule::Cron { expression: "* * * * *".into(), end_date_ms: None, max_occurrences: None },
                    true,
                    Status::Recurring,
                ),
                1..=3 => sample(
                    Schedule::Cron { expression: "0 * * * *".into(), end_date_ms: None, max_occurrences: None },
                    true,
                    Status::Recurring,
                ),
                _ => sample(
                    Schedule::Delay {
                        minutes: 30,
                        fire_at_ms: now_ms(now + Duration::minutes((i as i64 % 240) + 1)),
                    },
                    true,
                    Status::Pending,
                ),
            };
            reminder.id = format!("r{i}");
            reminder.created_at_ms = now_ms(now - Duration::days(3));
            reminder.last_occurrence_ms = Some(now_ms(now - Duration::minutes((i as i64 % 180) + 1)));
            reminders.push(reminder);
        }

        let started = Instant::now();
        let due = due_reminders(&reminders, now);
        let due_ms = started.elapsed();

        let started = Instant::now();
        let _ = next_wake(&reminders, now);
        let wake_ms = started.elapsed();

        let started = Instant::now();
        let timeline = today_timeline(&reminders, now);
        let timeline_ms = started.elapsed();

        let started = Instant::now();
        for expression in ["* * * * *", "0 */2 * * *", "0 9 * * 1-5"] {
            let times = crate::validate::next_cron_times(expression, now, 10).unwrap();
            assert_eq!(times.len(), 10);
        }
        let preview_ms = started.elapsed();

        eprintln!(
            "{total} 条：due={due_ms:?}（{} 命中）wake={wake_ms:?} timeline={timeline_ms:?}（{} 行）preview×3={preview_ms:?}",
            due.len(),
            timeline.len()
        );

        // 预算按 debug 构建给出的宽松上限，用来兜住 O(n²) 之类的回归；
        // release 构建大约快一个数量级（5000 条这一档 timeline 约几十毫秒）。
        assert!(due_ms.as_millis() < 1_000, "due_reminders 太慢：{due_ms:?}");
        assert!(wake_ms.as_millis() < 100, "next_wake 太慢：{wake_ms:?}");
        assert!(timeline_ms.as_millis() < 1_000, "today_timeline 太慢：{timeline_ms:?}");
        assert!(preview_ms.as_millis() < 100, "next_cron_times 太慢：{preview_ms:?}");
    }
}
