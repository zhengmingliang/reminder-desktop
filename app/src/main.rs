#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod cli;
mod glue;
mod hotkey;
mod instance;
mod logutil;
mod notify;
mod runtime;
mod session;
mod sni;
mod system_theme;
mod update;
mod xembed_tray;

use chrono::Local;
use reminder_core::{build_reminder, decode_bytes, next_cron_times, Draft, DEFAULT_CHIME};

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("桌面提醒：{error}");
            1
        }
    };
    std::process::exit(code);
}

const USAGE: &str = "\
用法：
  reminder add <标题> --in 5m [--body 正文] [--early 1m]   5 分钟后提醒（也可用 1h、2d、45）
  reminder add <标题> --at 09:30 [--early 10m]             今天 09:30（已过则顺延到明天）
  reminder add <标题> --cron \"0 9 * * 1-5\"                按 Cron 周期提醒
  --early 表示到点前先弹一次提示。直接运行 reminder 打开界面。

其它参数：
  --check         不打开窗口，做一遍自检后退出
  --smoke         打开窗口后马上退出
  --after-update  更新替换后由旧进程拉起，多等一会儿再判断有没有别的实例";

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--check") {
        self_check()?;
        println!("ok");
        return Ok(());
    }
    if cli::is_add(&args) {
        return cli::run(&args[1..]);
    }
    match args.first().map(String::as_str) {
        Some(token) if !token.starts_with('-') => {
            return Err(format!("未知子命令：{token}\n\n{USAGE}"));
        }
        _ => {}
    }
    glue::run(
        args.iter().any(|arg| arg == "--smoke"),
        args.iter().any(|arg| arg == "--after-update"),
    )
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
