//! `reminder add ...` 子命令：不启动界面，直接把一条提醒写进库。
//! 若已有界面实例在运行，则把提醒写成请求文件交给它处理，
//! 避免两个进程同时改同一个数据文件、互相覆盖。

use std::fs;
use std::path::Path;

use chrono::Local;
use reminder_core::{
    build_reminder, cli as cli_core, default_data_dir, millis_to_local, Draft, Reminder, Report,
    Schedule, Store,
};

use crate::instance;

/// 是否是本模块负责的命令行子命令。
pub fn is_add(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("add"))
}

/// 执行 `add` 子命令。`args` 是子命令名之后的参数。
pub fn run(args: &[String]) -> Result<(), String> {
    let draft = cli_core::parse_add(args, Local::now())?;
    let data_dir = default_data_dir();
    fs::create_dir_all(&data_dir).map_err(|error| format!("无法创建数据目录：{error}"))?;
    match instance::try_primary(&data_dir)? {
        // 没有实例在跑：直接写库。
        Some(_lock) => add_direct(&data_dir, &draft),
        // 有实例在跑：交给它去校验并落盘。
        None => add_handoff(&data_dir, &draft),
    }
}

fn add_direct(data_dir: &Path, draft: &Draft) -> Result<(), String> {
    let mut store = Store::open(data_dir.join("reminders.json"));
    let reminder =
        build_reminder(draft, None, Local::now(), store.reminders.len()).map_err(report_message)?;
    store.upsert(reminder.clone())?;
    println!("已添加提醒：{}（{}）", reminder.title, summarize(&reminder));
    Ok(())
}

fn add_handoff(data_dir: &Path, draft: &Draft) -> Result<(), String> {
    // 上限由正在运行的实例在真正落盘前再校验，这里只做基本校验、不传条数。
    let reminder = build_reminder(draft, None, Local::now(), 0).map_err(report_message)?;
    cli_core::append_add_request(&data_dir.join(cli_core::ADD_REQUEST_FILE), &reminder)
        .map_err(|error| format!("无法把提醒交给正在运行的实例：{error}"))?;
    println!(
        "已添加提醒：{}（{}），交由正在运行的实例处理",
        reminder.title,
        summarize(&reminder)
    );
    Ok(())
}

fn report_message(report: Report) -> String {
    report
        .errors
        .into_iter()
        .map(|error| error.message)
        .collect::<Vec<_>>()
        .join("；")
}

fn summarize(reminder: &Reminder) -> String {
    match &reminder.schedule {
        Schedule::Delay { minutes, .. } => format!("{minutes} 分钟后"),
        Schedule::At { fire_at_ms } => {
            format!("{}", millis_to_local(*fire_at_ms).format("%Y-%m-%d %H:%M"))
        }
        Schedule::Cron { expression } => format!("Cron {expression}"),
    }
}
