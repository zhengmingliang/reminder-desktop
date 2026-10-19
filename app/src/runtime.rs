use std::io::Cursor;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use chrono::Local;
use reminder_core::validate::{apply_fire, finish_snooze};
use reminder_core::{
    due_reminders, format_point, millis_to_local, next_wake, read_sound, DueHit, Reminder,
    Schedule, Store,
};
use rodio::{Decoder, OutputStream, Sink};

use crate::logutil::Logger;

#[derive(Clone, Debug)]
pub struct Notice {
    pub id: String,
    pub title: String,
    pub body: String,
    pub when_text: String,
    pub note: String,
    pub sound_enabled: bool,
    pub sound_path: String,
    pub snooze: bool,
    pub snooze_count: u32,
    pub kind_label: String,
    /// 计划时间已过才补发的（睡眠唤醒、改时间、程序没在跑）。
    pub late: bool,
    /// 被合并掉的更早次数。
    pub missed: u32,
}

pub struct Player {
    tx: Option<SyncSender<Vec<u8>>>,
    init_error: Option<String>,
}

impl Player {
    pub fn start(log: Arc<Logger>) -> Self {
        let (tx, rx) = mpsc::sync_channel(8);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("reminder-audio".into())
            .spawn(move || audio_loop(rx, ready_tx, log))
            .ok();
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Self { tx: Some(tx), init_error: None },
            Ok(Err(error)) => Self {
                tx: None,
                init_error: Some(error),
            },
            Err(_) => Self {
                tx: None,
                init_error: Some("音频设备初始化超时".into()),
            },
        }
    }

    pub fn play_bytes(&self, bytes: Vec<u8>) -> Result<(), String> {
        if let Some(error) = &self.init_error {
            return Err(error.clone());
        }
        let Some(tx) = &self.tx else {
            return Err("音频播放未就绪".into());
        };
        tx.try_send(bytes).map_err(|_| "铃声正在排队，请稍后再试".to_string())
    }

    pub fn play_reminder(&self, enabled: bool, path: &str) -> Result<(), String> {
        if !enabled {
            return Ok(());
        }
        let bytes = read_sound(&reminder_core::AudioSource::from_path(path))?;
        self.play_bytes(bytes)
    }
}

fn audio_loop(
    rx: Receiver<Vec<u8>>,
    ready: SyncSender<Result<(), String>>,
    log: Arc<Logger>,
) {
    let stream = match OutputStream::try_default() {
        Ok(stream) => {
            let _ = ready.send(Ok(()));
            stream
        }
        Err(error) => {
            let message = format!("没有可用的音频输出设备：{error}");
            log.log("WARN", &message);
            let _ = ready.send(Err(message));
            return;
        }
    };
    let (_stream, handle) = stream;
    while let Ok(bytes) = rx.recv() {
        match Decoder::new(Cursor::new(bytes)) {
            Ok(source) => match Sink::try_new(&handle) {
                Ok(sink) => {
                    sink.append(source);
                    sink.detach();
                }
                Err(error) => log.log("WARN", &format!("无法播放铃声：{error}")),
            },
            Err(error) => log.log("WARN", &format!("无法解码铃声：{error}")),
        }
    }
}

pub type Wake = Arc<(Mutex<()>, Condvar)>;

pub fn wake_new() -> Wake {
    Arc::new((Mutex::new(()), Condvar::new()))
}

pub fn poke(wake: &Wake) {
    let _guard = wake.0.lock().unwrap_or_else(|error| error.into_inner());
    wake.1.notify_one();
}

pub fn spawn_scheduler(
    store: Arc<Mutex<Store>>,
    wake: Wake,
    tx: mpsc::Sender<Vec<Notice>>,
    log: Arc<Logger>,
) {
    let _ = thread::Builder::new()
        .name("reminder-scheduler".into())
        .spawn(move || {
            loop {
                let tick_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch_due(&store, &tx, &log);
                }));
                if tick_result.is_err() {
                    log.log("ERROR", "调度线程发生异常，已恢复并继续");
                }
                let wait = wait_for(&store);
                let guard = wake.0.lock().unwrap_or_else(|error| error.into_inner());
                let _ = wake.1.wait_timeout(guard, wait);
            }
        });
}

fn dispatch_due(store: &Mutex<Store>, tx: &mpsc::Sender<Vec<Notice>>, log: &Logger) {
    let now = Local::now();
    let mut guard = store.lock().unwrap_or_else(|error| error.into_inner());
    let hits = due_reminders(&guard.reminders, now);
    if hits.is_empty() {
        return;
    }
    let mut notices = Vec::with_capacity(hits.len());
    for hit in hits {
        let Some(index) = guard.reminders.iter().position(|item| item.id == hit.id) else {
            continue;
        };
        let notice = notice_from(&guard.reminders[index], &hit, now);
        log.log(
            "INFO",
            &format!(
                "触发提醒 id={} title={} missed={}",
                notice.id, notice.title, hit.missed
            ),
        );
        if hit.snooze {
            finish_snooze(&mut guard.reminders[index], now);
        } else {
            apply_fire(&mut guard.reminders[index], hit.occurrence_ms, now);
        }
        notices.push(notice);
    }
    if let Err(error) = guard.save() {
        log.log("ERROR", &format!("触发后保存失败：{error}"));
    }
    drop(guard);
    if tx.send(notices).is_err() {
        log.log("ERROR", "界面已经退出，丢弃本次提醒弹窗");
    }
}

fn notice_from(reminder: &Reminder, hit: &DueHit, now: chrono::DateTime<Local>) -> Notice {
    let when = format_point(millis_to_local(hit.occurrence_ms), now, true);
    let note = if hit.snooze {
        "这是稍后提醒。".to_string()
    } else if hit.missed >= 100 {
        "未运行期间错过很多次，这次只显示最近的一次。".to_string()
    } else if hit.missed > 0 {
        format!("未运行期间错过 {} 次，这次只显示最近的一次。", hit.missed)
    } else if hit.late {
        "计划时间已过，这是补发的提醒。".to_string()
    } else {
        String::new()
    };
    Notice {
        id: reminder.id.clone(),
        title: reminder.title.clone(),
        body: reminder.body.clone(),
        when_text: format!("计划时间 {when}"),
        note,
        sound_enabled: reminder.sound_enabled,
        sound_path: reminder.sound_path.clone().unwrap_or_default(),
        snooze: hit.snooze,
        snooze_count: reminder.snooze_count,
        kind_label: match &reminder.schedule {
            Schedule::Delay { .. } => "延迟提醒",
            Schedule::At { .. } => "指定时间",
            Schedule::Cron { .. } => "Cron",
        }
        .to_string(),
        late: hit.late,
        missed: hit.missed,
    }
}

fn wait_for(store: &Mutex<Store>) -> Duration {
    let now = Local::now();
    let guard = store.lock().unwrap_or_else(|error| error.into_inner());
    let Some(next) = next_wake(&guard.reminders, now) else {
        return Duration::from_secs(5);
    };
    drop(guard);
    if next <= now {
        return Duration::from_millis(80);
    }
    let Ok(distance) = (next - now).to_std() else {
        return Duration::from_secs(5);
    };
    if distance < Duration::from_secs(2) {
        Duration::from_millis(100)
    } else {
        (distance - Duration::from_secs(1)).min(Duration::from_secs(5))
    }
}
