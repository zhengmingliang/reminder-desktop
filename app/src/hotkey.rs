//! X11 全局快捷键：在根窗口上抓键，命中后通过 channel 通知主循环。
//! 纯 Wayland 会话协议上抓不到全局键，spawn 直接报错，由上层降级为仅托盘操作。
//! 抓哪两个组合键由调用方给（默认 Ctrl+Alt+R 呼出主窗口、Ctrl+Alt+N 快速新建）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use crate::logutil::Logger;
use reminder_core::Hotkey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyCommand {
    ToggleMain,
    QuickNew,
}

pub struct HotkeyHandle {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HotkeyHandle {
    /// 通知抓键线程退出；连接断开时 X 服务器会自动放开抓键。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 启动抓键线程。抓键在线程里做，spawn 会等到抓键结果再返回，
/// 避免设置里显示「已启用」但组合键其实被别的程序占着。
#[cfg(all(unix, not(target_os = "macos")))]
pub fn spawn(
    tx: mpsc::Sender<HotkeyCommand>,
    log: Arc<Logger>,
    main: Hotkey,
    quick_new: Hotkey,
) -> Result<HotkeyHandle, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return Err("Wayland 会话不支持全局快捷键".into());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("reminder-hotkey".into())
        .spawn(move || run(tx, log, stop_thread, ready_tx, main, quick_new))
        .map_err(|error| format!("无法启动快捷键线程：{error}"))?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(HotkeyHandle {
            stop,
            thread: Some(thread),
        }),
        Ok(Err(message)) => {
            let _ = thread.join();
            Err(message)
        }
        Err(_) => Err("快捷键线程异常退出".into()),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn run(
    tx: mpsc::Sender<HotkeyCommand>,
    log: Arc<Logger>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
    main: Hotkey,
    quick_new: Hotkey,
) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
    use x11rb::protocol::Event;

    let bindings = [(main, HotkeyCommand::ToggleMain), (quick_new, HotkeyCommand::QuickNew)];

    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(pair) => pair,
        Err(error) => {
            let _ = ready.send(Err(format!("无法连接 X 服务器：{error}")));
            return;
        }
    };
    let setup = conn.setup();
    let Some(screen) = setup.roots.get(screen_num) else {
        let _ = ready.send(Err("找不到 X 屏幕".into()));
        return;
    };
    let root = screen.root;
    let (lo, hi) = (setup.min_keycode, setup.max_keycode);
    let keycode_of = |keysym: u32| -> Option<u8> {
        let reply = conn
            .get_keyboard_mapping(lo, hi.saturating_sub(lo).saturating_add(1))
            .ok()?
            .reply()
            .ok()?;
        let per = usize::from(reply.keysyms_per_keycode).max(1);
        reply
            .keysyms
            .chunks(per)
            .position(|chunk| chunk.contains(&keysym))
            .map(|index| lo.saturating_add(index as u8))
    };
    // 每个组合键：拿到它的修饰位、主键 keycode，以及回显用的文本。
    let mut grabbed: Vec<(HotkeyCommand, u16, u8, String)> = Vec::new();
    for (hotkey, command) in bindings {
        let mut mods = ModMask::from(0u16);
        if hotkey.ctrl {
            mods |= ModMask::CONTROL;
        }
        if hotkey.alt {
            mods |= ModMask::M1;
        }
        if hotkey.shift {
            mods |= ModMask::SHIFT;
        }
        if hotkey.logo {
            mods |= ModMask::M4;
        }
        let Some(keycode) = keycode_of(hotkey.key.keysym()) else {
            let _ = ready.send(Err(format!(
                "键盘布局里找不到 {} 这个键",
                hotkey.text()
            )));
            return;
        };
        // CapsLock / NumLock 按下时修饰状态位不同，四种组合都抓，否则锁定时快捷键会失灵。
        for extra in [
            ModMask::from(0u16),
            ModMask::LOCK,
            ModMask::M2,
            ModMask::LOCK | ModMask::M2,
        ] {
            match conn.grab_key(
                false,
                root,
                mods | extra,
                keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            ) {
                Ok(cookie) => {
                    if cookie.check().is_err() {
                        let _ = ready.send(Err(format!(
                            "快捷键 {} 被其他程序占用",
                            hotkey.text()
                        )));
                        return;
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(format!("抓取快捷键失败：{error}")));
                    return;
                }
            }
        }
        grabbed.push((command, u16::from(mods), keycode, hotkey.text()));
    }
    let _ = conn.flush();
    log.log(
        "INFO",
        &format!(
            "全局快捷键已启用：{} 呼出主窗口，{} 快速新建",
            grabbed[0].3, grabbed[1].3
        ),
    );
    let _ = ready.send(Ok(()));

    // CapsLock / NumLock 的位不计入匹配，否则锁定状态下快捷键会失灵。
    let ignore = u16::from(ModMask::LOCK) | u16::from(ModMask::M2);
    while !stop.load(Ordering::Relaxed) {
        match conn.poll_for_event() {
            Ok(Some(Event::KeyPress(press))) => {
                let state = u16::from(press.state) & !ignore;
                let hit = grabbed
                    .iter()
                    .find(|(_, mods, keycode, _)| *mods == state && *keycode == press.detail)
                    .map(|(command, ..)| *command);
                if let Some(command) = hit {
                    if tx.send(command).is_err() {
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                log.log("WARN", &format!("全局快捷键监听中断：{error}"));
                break;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    // 退出前放开抓键（AnyModifier），把组合键还给系统。
    for (_, _, keycode, _) in &grabbed {
        let _ = conn.ungrab_key(*keycode, root, ModMask::from(1u16 << 15));
    }
    let _ = conn.flush();
    log.log("INFO", "全局快捷键已停用");
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn spawn(
    _tx: mpsc::Sender<HotkeyCommand>,
    _log: Arc<Logger>,
    _main: Hotkey,
    _quick_new: Hotkey,
) -> Result<HotkeyHandle, String> {
    Err("当前平台暂不支持全局快捷键".into())
}
