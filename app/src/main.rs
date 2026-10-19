#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod glue;
mod instance;
mod logutil;
mod runtime;
mod system_theme;
mod xembed_tray;

use chrono::Local;
use reminder_core::{build_reminder, decode_bytes, next_cron_times, Draft, DEFAULT_CHIME};

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("桌面提醒启动失败：{error}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<(), String> {
    if std::env::args().any(|arg| arg == "--check") {
        self_check()?;
        println!("ok");
        return Ok(());
    }
    glue::run(std::env::args().any(|arg| arg == "--smoke"))
}

fn self_check() -> Result<(), String> {
    let now = Local::now();
    let mut draft = Draft::blank();
    draft.title = "自检".into();
    draft.minutes = "1".into();
    build_reminder(&draft, None, now, 0).map_err(|report| {
        report
            .errors
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>()
            .join("；")
    })?;
    let times = next_cron_times("0 9 * * 1-5", now, 10)?;
    if times.len() != 10 {
        return Err("Cron 预览没有算出 10 次".into());
    }
    decode_bytes(DEFAULT_CHIME)?;
    Ok(())
}
