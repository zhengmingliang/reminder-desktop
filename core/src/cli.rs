//! 命令行子命令的纯逻辑：把 `add` 的参数解析成表单草稿，并在界面实例已经在运行时，
//! 用「请求文件」把待添加的提醒交给它处理（避免两个进程同时改同一个数据文件）。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local, NaiveTime};

use crate::model::{Draft, Kind, Reminder};

/// 解析 `add` 子命令的参数（不含子命令名本身），生成一份表单草稿。
///
/// 支持 `--in 5m|1h|2d|45`、`--at HH:MM`、`--cron "表达式"` 指定触发方式（三选一），
/// `--body 正文` 填正文，其余非选项词按顺序拼成标题。
pub fn parse_add(args: &[String], now: DateTime<Local>) -> Result<Draft, String> {
    let mut draft = Draft::blank();
    let mut title_parts: Vec<String> = Vec::new();
    let mut delay: Option<String> = None;
    let mut at: Option<String> = None;
    let mut cron: Option<String> = None;
    let mut body: Option<String> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--in" => delay = Some(take_value(args, &mut index, "--in")?),
            "--at" => at = Some(take_value(args, &mut index, "--at")?),
            "--cron" => cron = Some(take_value(args, &mut index, "--cron")?),
            "--body" => body = Some(take_value(args, &mut index, "--body")?),
            other if other.starts_with("--") => return Err(format!("未知选项：{other}")),
            other => title_parts.push(other.to_string()),
        }
        index += 1;
    }

    let title = title_parts.join(" ");
    if title.trim().is_empty() {
        return Err("请给出提醒标题，例如：reminder add 喝水 --in 5m".into());
    }
    draft.title = title;

    let chosen = usize::from(delay.is_some()) + usize::from(at.is_some()) + usize::from(cron.is_some());
    if chosen == 0 {
        return Err("请用 --in、--at 或 --cron 指定触发时间".into());
    }
    if chosen > 1 {
        return Err("--in、--at、--cron 只能用其中一个".into());
    }

    if let Some(spec) = delay {
        draft.kind = Kind::Delay;
        draft.minutes = parse_duration_minutes(&spec)?.to_string();
    } else if let Some(spec) = at {
        let (date, time) = resolve_at(&spec, now)?;
        draft.kind = Kind::At;
        draft.date = date;
        draft.time = time;
    } else if let Some(spec) = cron {
        draft.kind = Kind::Cron;
        draft.cron = spec;
    }
    if let Some(text) = body {
        draft.body = text;
    }
    Ok(draft)
}

/// 取下一个参数作为选项值，并把游标推到该值上（外层循环紧接着再 +1 跳过它）。
fn take_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} 缺少值"))
}

/// `--in` 的时长：`45`、`5m`、`1h`、`2d` 换算成分钟。
pub fn parse_duration_minutes(spec: &str) -> Result<u32, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("--in 需要一个时长，例如 5m、1h、2d".into());
    }
    let digits_len = spec.chars().take_while(|ch| ch.is_ascii_digit()).count();
    let (digits, unit) = spec.split_at(digits_len);
    if digits.is_empty() {
        return Err(format!("时长格式不对：{spec}，例如 5m、1h、2d"));
    }
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("时长数字太大：{digits}"))?;
    let minutes = match unit.to_ascii_lowercase().as_str() {
        "" | "m" | "min" | "mins" => value,
        "h" | "hr" | "hour" | "hours" => value.saturating_mul(60),
        "d" | "day" | "days" => value.saturating_mul(60 * 24),
        other => return Err(format!("不认识的时间单位：{other}（可用 m / h / d）")),
    };
    u32::try_from(minutes).map_err(|_| "时长太大".to_string())
}

/// `--at HH:MM`：今天这个点还没到就用今天，已经过了就顺延到明天。
fn resolve_at(spec: &str, now: DateTime<Local>) -> Result<(String, String), String> {
    let cleaned = spec.trim().replace('：', ":");
    let time = NaiveTime::parse_from_str(&cleaned, "%H:%M")
        .map_err(|_| format!("--at 需要 HH:MM 形式，例如 09:30（收到 {spec}）"))?;
    let today = now.date_naive();
    let candidate = today.and_time(time);
    let date = if candidate > now.naive_local() {
        today
    } else {
        today + Duration::days(1)
    };
    Ok((date.format("%Y-%m-%d").to_string(), time.format("%H:%M").to_string()))
}

pub const ADD_REQUEST_FILE: &str = "add.request";

/// 把待添加的提醒追加到请求文件（每行一条 JSON）。
/// 用追加而不是覆盖，避免连续添加时把上一条挤掉。
pub fn append_add_request(path: &Path, reminder: &Reminder) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(reminder).unwrap_or_default();
    line.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)
}

/// 取走请求文件里的所有提醒。先把文件改名再读，避免和正在追加的进程抢同一份文件。
pub fn take_add_requests(path: &Path) -> Vec<Reminder> {
    let staging = processing_path(path);
    if std::fs::rename(path, &staging).is_err() {
        return Vec::new();
    }
    let bytes = std::fs::read(&staging).unwrap_or_default();
    let _ = std::fs::remove_file(&staging);
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

fn processing_path(path: &Path) -> PathBuf {
    path.with_extension("processing")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 3, 4, 10, 0, 0).unwrap()
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn duration_supports_bare_minutes_and_units() {
        assert_eq!(parse_duration_minutes("45").unwrap(), 45);
        assert_eq!(parse_duration_minutes("5m").unwrap(), 5);
        assert_eq!(parse_duration_minutes("1h").unwrap(), 60);
        assert_eq!(parse_duration_minutes("2d").unwrap(), 2880);
        assert!(parse_duration_minutes("5x").is_err());
        assert!(parse_duration_minutes("abc").is_err());
    }

    #[test]
    fn add_parses_title_and_delay() {
        let draft = parse_add(&args(&["喝水", "--in", "5m"]), now()).unwrap();
        assert_eq!(draft.title, "喝水");
        assert_eq!(draft.kind, Kind::Delay);
        assert_eq!(draft.minutes, "5");
    }

    #[test]
    fn add_joins_unquoted_title_words_and_takes_body() {
        let draft = parse_add(&args(&["喝", "水", "--in", "30", "--body", "多喝点"]), now()).unwrap();
        assert_eq!(draft.title, "喝 水");
        assert_eq!(draft.minutes, "30");
        assert_eq!(draft.body, "多喝点");
    }

    #[test]
    fn add_rejects_missing_or_conflicting_schedule() {
        assert!(parse_add(&args(&["喝水"]), now()).is_err());
        assert!(parse_add(&args(&["喝水", "--in", "5m", "--at", "09:00"]), now()).is_err());
        assert!(parse_add(&args(&["--in", "5m"]), now()).is_err());
    }

    #[test]
    fn at_rolls_to_tomorrow_when_already_passed() {
        let (date, time) = resolve_at("09:00", now()).unwrap();
        assert_eq!(date, "2026-03-05");
        assert_eq!(time, "09:00");
        let (date, time) = resolve_at("18:30", now()).unwrap();
        assert_eq!(date, "2026-03-04");
        assert_eq!(time, "18:30");
    }

    #[test]
    fn add_request_roundtrips_through_ndjson() {
        let draft = parse_add(&args(&["喝水", "--in", "5m"]), now()).unwrap();
        let reminder = crate::build_reminder(&draft, None, now(), 0).unwrap();
        let dir = std::env::temp_dir().join(format!("rd-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(ADD_REQUEST_FILE);
        append_add_request(&path, &reminder).unwrap();
        append_add_request(&path, &reminder).unwrap();
        let taken = take_add_requests(&path);
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].title, "喝水");
        // 取走后文件应被移走，不留残留。
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
