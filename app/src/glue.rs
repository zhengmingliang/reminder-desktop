use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, Offset, Timelike};
use reminder_core::validate::arm_snooze;
use reminder_core::{
    check_sound_file, format_point, inspect, millis_to_local, Draft, Kind, Reminder, Report,
    Status, Store,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};

use crate::instance::{self, Launch};
use crate::logutil::Logger;
use crate::runtime::{self, Notice, Player, Wake};
use crate::xembed_tray::{self, TrayCommand};

slint::include_modules!();

thread_local! {
    static TRAY_POPUP: Cell<bool> = const { Cell::new(false) };
}

enum Nav {
    New,
    Select(String),
}

struct PopupSlot {
    id: u64,
    window: PopupWindow,
    _timers: Vec<slint::Timer>,
}

struct Session {
    ui: Weak<MainWindow>,
    tray: RefCell<Option<AppTray>>,
    /// XEmbed 托盘线程的消息发送端：用来切换「有稍后提醒」的角标。
    tray_icon: RefCell<Option<std::sync::mpsc::Sender<xembed_tray::TrayIconMsg>>>,
    menu: RefCell<Option<TrayMenu>>,
    store: Arc<Mutex<Store>>,
    rows: Rc<VecModel<ReminderRow>>,
    dirs: Rc<VecModel<DirEntry>>,
    wake: Wake,
    player: Player,
    log: Arc<Logger>,
    data_dir: PathBuf,
    browser_dir: RefCell<PathBuf>,
    selected: RefCell<Option<String>>,
    loaded: RefCell<Option<Reminder>>,
    baseline: RefCell<Draft>,
    sound_path: RefCell<String>,
    search: RefCell<String>,
    applying: Cell<bool>,
    pending: RefCell<Option<Nav>>,
    popups: Rc<RefCell<Vec<PopupSlot>>>,
    popup_seq: Cell<u64>,
    sound_cache: RefCell<(String, String)>,
    reveal_errors: Cell<bool>,
    backups: RefCell<Vec<PathBuf>>,
    pending_restore: RefCell<Option<PathBuf>>,
}

pub fn run(smoke: bool) -> Result<(), String> {
    let data_dir = reminder_core::default_data_dir();
    fs::create_dir_all(&data_dir).map_err(|error| format!("无法创建数据目录：{error}"))?;
    let log = Arc::new(Logger::open(data_dir.join("app.log")));
    log.log("INFO", "启动");

    let lock = match instance::claim(&data_dir)? {
        Launch::Secondary => {
            println!("桌面提醒已在运行，已请求打开主窗口");
            log.log("INFO", "已有实例在运行，本次退出");
            return Ok(());
        }
        Launch::Primary(lock) => lock,
    };

    let previous = std::panic::take_hook();
    let panic_log = log.clone();
    std::panic::set_hook(Box::new(move |info| {
        panic_log.log("ERROR", &format!("程序异常：{info}"));
        previous(info);
    }));

    select_backend()?;

    let store = Arc::new(Mutex::new(Store::open(data_dir.join("reminders.json"))));
    let ui = MainWindow::new().map_err(|error| format!("无法创建主窗口：{error}"))?;
    let (tray_tx, tray_rx) = mpsc::channel::<TrayCommand>();
    let (icon_tx, xembed) = match xembed_tray::spawn(tray_tx) {
        Ok(tx) => {
            log.log("INFO", "已注册 XEmbed 托盘图标");
            (Some(tx), true)
        }
        Err(error) => {
            log.log("WARN", &format!("XEmbed 托盘不可用：{error}"));
            (None, false)
        }
    };
    let slint_tray = if xembed {
        None
    } else {
        match AppTray::new() {
            Ok(tray) => {
                let _ = tray.show();
                log.log("INFO", "已注册 StatusNotifier 托盘图标");
                Some(tray)
            }
            Err(error) => {
                log.log("ERROR", &format!("托盘图标创建失败：{error}"));
                None
            }
        }
    };
    ui.set_ui_font(ui_font().into());
    ui.set_mono_font(mono_font().into());
    ui.set_autostart_on(crate::autostart::is_enabled());

    let rows = Rc::new(VecModel::<ReminderRow>::default());
    let dirs = Rc::new(VecModel::<DirEntry>::default());
    ui.set_rows(rows.clone().into());
    ui.set_dir_entries(dirs.clone().into());

    let session = Rc::new(Session {
        ui: ui.as_weak(),
        tray: RefCell::new(slint_tray),
        tray_icon: RefCell::new(icon_tx),
        menu: RefCell::new(None),
        store: store.clone(),
        rows,
        dirs,
        wake: runtime::wake_new(),
        player: Player::start(log.clone()),
        log: log.clone(),
        data_dir: data_dir.clone(),
        browser_dir: RefCell::new(home_dir()),
        selected: RefCell::new(None),
        loaded: RefCell::new(None),
        baseline: RefCell::new(Draft::blank()),
        sound_path: RefCell::new(String::new()),
        search: RefCell::new(String::new()),
        applying: Cell::new(false),
        pending: RefCell::new(None),
        popups: Rc::new(RefCell::new(Vec::new())),
        popup_seq: Cell::new(1),
        sound_cache: RefCell::new((String::new(), String::new())),
        reveal_errors: Cell::new(false),
        backups: RefCell::new(Vec::new()),
        pending_restore: RefCell::new(None),
    });

    if let Some(reason) = lock_store(&store).persist_block_reason().map(str::to_string) {
        session.toast(&reason);
        log.log("ERROR", &reason);
    }

    wire(&ui, &session);
    session.show_blank();
    session.refresh_list();
    session.refresh_backups();
    session.tick_clock();

    // 视觉走查辅助：可指定初始视图与提醒类型，例如 REMINDER_UI_VIEW=edit REMINDER_UI_KIND=at。
    if let Ok(view) = std::env::var("REMINDER_UI_VIEW") {
        ui.set_main_view(view.into());
    }
    if let Ok(kind) = std::env::var("REMINDER_UI_KIND") {
        ui.set_kind(kind.into());
    }

    // 视觉走查辅助：设置该环境变量时启动即弹出预览弹窗（不落库、不改数据）。
    // 草稿字段在定时器里现填现弹，避免被启动期间的刷新覆盖。
    if std::env::var_os("REMINDER_PREVIEW_POPUP").is_some() {
        let session = session.clone();
        slint::Timer::single_shot(Duration::from_millis(600), move || {
            if let Some(ui) = session.ui.upgrade() {
                ui.set_form_title("晚上记得打电话".into());
                ui.set_form_body("cd reminder-desktop\ncargo test -p reminder-core\ncargo build --release -p reminder-desktop".into());
                ui.set_form_minutes("5".into());
            }
            session.preview_popup();
        });
    }

    let (notice_tx, notice_rx) = mpsc::channel::<Vec<Notice>>();
    let timer = slint::Timer::default();
    let timed = session.clone();
    let notice_rx_timer = Rc::new(RefCell::new(notice_rx));
    let tray_rx = Rc::new(RefCell::new(tray_rx));
    let tray_rx_timer = tray_rx.clone();
    let mut ticks = 0u32;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(200), move || {
        ticks = ticks.wrapping_add(1);
        if ticks % 5 == 0 {
            timed.tick_clock();
            timed.refresh_list();
        }
        timed.poll_show_request();
        let batch: Vec<Vec<Notice>> = notice_rx_timer.borrow_mut().try_iter().collect();
        for notices in batch {
            timed.show_notices(notices);
        }
        let commands: Vec<TrayCommand> = tray_rx_timer.borrow_mut().try_iter().collect();
        for command in commands {
            match command {
                TrayCommand::Show => timed.toggle_main(),
                TrayCommand::Menu(anchor) => timed.open_tray_menu(anchor),
            }
        }
    });

    ui.show().map_err(|error| format!("无法显示主窗口：{error}"))?;
    place_main(ui.window());
    runtime::spawn_scheduler(store, session.wake.clone(), notice_tx, log.clone());

    let smoke_timer = slint::Timer::default();
    if smoke {
        smoke_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(900), || {
            let _ = slint::quit_event_loop();
        });
    }
    let center_timer = slint::Timer::default();
    let center_ui = ui.as_weak();
    center_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(160), move || {
        if let Some(ui) = center_ui.upgrade() {
            place_main(ui.window());
        }
    });

    slint::run_event_loop_until_quit().map_err(|error| format!("事件循环退出：{error}"))?;
    log.log("INFO", "退出");
    drop(lock);
    drop(timer);
    drop(smoke_timer);
    drop(center_timer);
    Ok(())
}

fn wire(ui: &MainWindow, session: &Rc<Session>) {        ui.window().on_close_requested({
        let session = session.clone();
        move || {
            session.update_tip();
            session.log.log("INFO", "主窗口关闭，转入后台");
            slint::CloseRequestResponse::HideWindow
        }
    });

    ui.on_draft_edited({
        let session = session.clone();
        move || {
            session.reveal_errors.set(true);
            session.refresh_validation();
        }
    });
    ui.on_save({
        let session = session.clone();
        move || session.save()
    });
    ui.on_new_reminder({
        let session = session.clone();
        move || session.request_nav(Nav::New)
    });
    ui.on_select_reminder({
        let session = session.clone();
        move |id| session.request_nav(Nav::Select(id.to_string()))
    });
    ui.on_delete_request({
        let session = session.clone();
        move || session.ask_delete()
    });
    ui.on_dialog_confirm({
        let session = session.clone();
        move || session.confirm_dialog()
    });
    ui.on_dialog_cancel({
        let session = session.clone();
        move || session.close_dialog()
    });
    ui.on_hide_to_tray({
        let session = session.clone();
        move || session.hide_main()
    });
    ui.on_open_settings({
        let session = session.clone();
        move || session.open_settings()
    });
    ui.on_autostart_changed({
        let session = session.clone();
        move |on| session.set_autostart(on)
    });
    ui.on_quit({
        let session = session.clone();
        move || {
            session.log.log("INFO", "用户退出");
            let _ = slint::quit_event_loop();
        }
    });
    ui.on_backup({
        let session = session.clone();
        move || session.backup()
    });
    ui.on_restore_backup({
        let session = session.clone();
        move |index| session.ask_restore(index)
    });
    ui.on_preview_sound({
        let session = session.clone();
        move || session.preview_sound()
    });
    ui.on_browse_open({
        let session = session.clone();
        move || session.open_browser()
    });
    ui.on_browse_cancel({
        let session = session.clone();
        move || {
            if let Some(ui) = session.ui.upgrade() {
                ui.set_browser_open(false);
            }
        }
    });
    ui.on_browse_enter({
        let session = session.clone();
        move |path, kind| session.browse_enter(path.to_string(), kind.to_string())
    });
    ui.on_browse_use({
        let session = session.clone();
        move || session.browse_use()
    });
    ui.on_use_default_sound({
        let session = session.clone();
        move || session.use_default_sound()
    });
    ui.on_search_changed({
        let session = session.clone();
        move |text| {
            *session.search.borrow_mut() = text.to_string();
            session.refresh_list();
        }
    });
    ui.on_kind_changed({
        let session = session.clone();
        move |kind| {
            if session.applying.get() {
                return;
            }
            if let Some(ui) = session.ui.upgrade() {
                ui.set_kind(kind);
            }
            session.reveal_errors.set(true);
            session.refresh_validation();
        }
    });
    ui.on_preset({
        let session = session.clone();
        move |expression| {
            if let Some(ui) = session.ui.upgrade() {
                ui.set_kind("cron".into());
                ui.set_form_cron(expression);
            }
            session.reveal_errors.set(true);
            session.refresh_validation();
        }
    });
    ui.on_filter_changed({
        let session = session.clone();
        move |_| session.refresh_list()
    });
    ui.on_minutes_step({
        let session = session.clone();
        move |delta| session.step_minutes(delta)
    });
    ui.on_minutes_preset({
        let session = session.clone();
        move |minutes| session.preset_minutes(minutes)
    });        ui.on_revert({
            let session = session.clone();
            move || session.revert()
        });
        ui.on_quick_create({
            let session = session.clone();
            move |title, preset| session.quick_create(title.to_string(), i32::from(preset))
        });
        ui.on_at_preset({
            let session = session.clone();
            move |index| session.at_preset(i32::from(index))
        });
        ui.on_cron_time_changed({
            let session = session.clone();
            move |text| session.cron_time_changed(text.to_string())
        });
        ui.on_preview_popup({
            let session = session.clone();
            move || session.preview_popup()
        });
        ui.on_minimize_window({
            let session = session.clone();
            move || session.minimize_main()
        });
        ui.on_maximize_window({
            let session = session.clone();
            move || session.maximize_main()
        });

    if let Some(tray) = session.tray.borrow().as_ref() {
        tray.on_show_window({
            let session = session.clone();
            move || session.show_main()
        });
        tray.on_toggle_window({
            let session = session.clone();
            move || session.toggle_main()
        });
        tray.on_new_reminder({
            let session = session.clone();
            move || session.new_reminder_from_tray()
        });
        tray.on_backup({
            let session = session.clone();
            move || session.backup_from_tray()
        });
        tray.on_open_settings({
            let session = session.clone();
            move || session.open_settings()
        });
        tray.on_quit_app({
            let session = session.clone();
            move || session.quit_from_tray()
        });
    }
}

impl Session {
    fn show_blank(&self) {
        *self.loaded.borrow_mut() = None;
        *self.selected.borrow_mut() = None;
        *self.sound_path.borrow_mut() = String::new();
        self.reveal_errors.set(false);
        let blank = Draft::blank();
        *self.baseline.borrow_mut() = blank.clone();
        self.write_draft(&blank);
    }

    fn request_nav(&self, nav: Nav) {
        if self.is_dirty() {
            *self.pending.borrow_mut() = Some(nav);
            self.open_dialog(
                "放弃未保存的修改？",
                "当前编辑还没有保存。离开后，这些修改会被丢掉。",
            );
            if let Some(ui) = self.ui.upgrade() {
                ui.set_dialog("discard".into());
            }
            return;
        }
        self.apply_nav(nav);
    }

    fn apply_nav(&self, nav: Nav) {
        match nav {
            Nav::New => {
                self.show_blank();
                if let Some(ui) = self.ui.upgrade() {
                    ui.set_main_view("edit".into());
                }
            }
            Nav::Select(id) => self.load_id(&id),
        }
        self.refresh_list();
    }

    fn load_id(&self, id: &str) {
        let Some(reminder) = lock_store(&self.store)
            .reminders
            .iter()
            .find(|item| item.id == id)
            .cloned()
        else {
            self.toast("这条提醒已经不存在");
            self.show_blank();
            return;
        };
        self.reveal_errors.set(true);
        *self.selected.borrow_mut() = Some(id.to_string());
        *self.sound_path.borrow_mut() = reminder.sound_path.clone().unwrap_or_default();
        let draft = reminder.to_draft();
        *self.baseline.borrow_mut() = draft.clone();
        *self.loaded.borrow_mut() = Some(reminder);
        self.write_draft(&draft);
    }

    fn write_draft(&self, draft: &Draft) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        self.applying.set(true);
        ui.set_kind(draft.kind.as_str().into());
        ui.set_form_title(draft.title.clone().into());
        ui.set_form_body(draft.body.clone().into());
        ui.set_form_minutes(draft.minutes.clone().into());
        ui.set_form_date(draft.date.clone().into());
        ui.set_form_time(draft.time.clone().into());
        ui.set_form_cron(draft.cron.clone().into());
        ui.set_form_sound(draft.sound_enabled);
        ui.set_form_enabled(draft.enabled);
        ui.set_sound_label(sound_label(&self.sound_path.borrow()).into());
        ui.set_delete_enabled(self.loaded.borrow().is_some());
        ui.set_editor_heading(if self.loaded.borrow().is_some() {
            "编辑提醒"
        } else {
            "新建提醒"
        }.into());
        self.applying.set(false);
        self.refresh_validation();
    }

    fn read_draft(&self) -> Draft {
        let Some(ui) = self.ui.upgrade() else {
            return Draft::blank();
        };
        Draft {
            id: self.loaded.borrow().as_ref().map(|item| item.id.clone()),
            title: ui.get_form_title().to_string(),
            body: ui.get_form_body().to_string(),
            kind: Kind::parse(ui.get_kind().as_str()),
            minutes: ui.get_form_minutes().to_string(),
            date: ui.get_form_date().to_string(),
            time: ui.get_form_time().to_string(),
            cron: ui.get_form_cron().to_string(),
            sound_enabled: ui.get_form_sound(),
            sound_path: self.sound_path.borrow().clone(),
            enabled: ui.get_form_enabled(),
        }
    }

    fn is_dirty(&self) -> bool {
        let mut current = self.read_draft();
        current.id = self.baseline.borrow().id.clone();
        current != *self.baseline.borrow()
    }

    fn refresh_validation(&self) {
        if self.applying.get() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let draft = self.read_draft();
        let mut report = inspect(&draft, Local::now());
        if !self.reveal_errors.get() {
            report.errors.clear();
        }
        let sound_error = self.sound_error(&draft.sound_path);
        if !sound_error.is_empty() {
            report.errors.push(reminder_core::FieldError {
                field: "sound",
                message: sound_error,
            });
        }
        show_report(&ui, &report);
        let cron = draft.cron.trim().to_string();
        let head = if draft.kind.as_str() == "cron" && !cron.is_empty() {
            format!("将按 {cron} 重复提醒")
        } else {
            String::new()
        };
        ui.set_preview_head(head.into());
        ui.set_banner(banner_text(self.loaded.borrow().as_ref(), &draft).into());
        ui.set_history_hint(history_text(self.loaded.borrow().as_ref()).into());
        ui.set_title_count(
            format!(
                "{} / {}",
                draft.title.chars().count(),
                reminder_core::model::TITLE_MAX
            )
            .into(),
        );
        ui.set_body_count(
            format!(
                "{} / {}",
                draft.body.chars().count(),
                reminder_core::model::BODY_MAX
            )
            .into(),
        );
    }

    fn step_minutes(&self, delta: i32) {
        let current = self.read_draft().minutes.trim().parse::<i32>().unwrap_or(0);
        let next = (current + delta).clamp(1, reminder_core::model::DELAY_MAX as i32);
        self.set_minutes_value(next);
    }

    fn preset_minutes(&self, minutes: i32) {
        let next = minutes.clamp(1, reminder_core::model::DELAY_MAX as i32);
        self.set_minutes_value(next);
    }

    fn set_minutes_value(&self, minutes: i32) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_kind("delay".into());
        ui.set_form_minutes(minutes.to_string().into());
        self.reveal_errors.set(true);
        self.refresh_validation();
    }

    fn revert(&self) {
        let draft = self.baseline.borrow().clone();
        *self.sound_path.borrow_mut() = draft.sound_path.clone();
        self.write_draft(&draft);
    }

    fn sound_error(&self, path: &str) -> String {
        let key = path.trim().to_string();
        let mut cache = self.sound_cache.borrow_mut();
        if cache.0 != key {
            let message = check_sound_file(&key).err().unwrap_or_default();
            *cache = (key, message);
        }
        cache.1.clone()
    }

    fn save(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let draft = self.read_draft();
        let now = Local::now();
        let mut store = lock_store(&self.store);
        if let Some(reason) = store.persist_block_reason().map(str::to_string) {
            drop(store);
            self.toast(&reason);
            return;
        }
        let previous = draft.id.as_ref().and_then(|id| {
            store.reminders.iter().find(|item| &item.id == id).cloned()
        });
        let count = store.reminders.len();
        let reminder = match build_reminder_mapped(&draft, previous.as_ref(), now, count) {
            Ok(reminder) => reminder,
            Err(report) => {
                show_report(&ui, &report);
                ui.set_form_error("请先修正标出的内容".into());
                return;
            }
        };
        if let Err(error) = check_sound_file(reminder.sound_path.as_deref().unwrap_or("")) {
            ui.set_sound_error(error.into());
            ui.set_form_error("请先修正标出的内容".into());
            return;
        }
        let id = reminder.id.clone();
        if let Err(error) = store.upsert(reminder.clone()) {
            drop(store);
            self.toast(&error);
            return;
        }
        drop(store);
        runtime::poke(&self.wake);
        *self.selected.borrow_mut() = Some(id.clone());
        *self.sound_path.borrow_mut() = reminder.sound_path.clone().unwrap_or_default();
        let draft = reminder.to_draft();
        *self.baseline.borrow_mut() = draft.clone();
        *self.loaded.borrow_mut() = Some(reminder);
        self.write_draft(&draft);
        self.refresh_list();
        self.toast("已保存");
        self.log.log("INFO", &format!("保存提醒 id={id}"));
    }

    fn ask_delete(&self) {
        let Some(id) = self.loaded.borrow().as_ref().map(|item| item.id.clone()) else {
            return;
        };
        let title = self.loaded.borrow().as_ref().map(|item| item.title.clone()).unwrap_or(id);
        self.open_dialog("删除这条提醒？", &format!("「{title}」删除后不能恢复。"));
        if let Some(ui) = self.ui.upgrade() {
            ui.set_dialog("delete".into());
        }
    }

    fn confirm_dialog(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let kind = ui.get_dialog().to_string();
        ui.set_dialog("".into());
        if kind == "delete" {
            self.delete_loaded();
            return;
        }
        if kind == "quit" {
            self.quit();
            return;
        }
        if kind == "restore" {
            self.restore_backup();
            return;
        }
        if kind == "discard" {
            if let Some(nav) = self.pending.borrow_mut().take() {
                self.apply_nav(nav);
            }
        }
    }

    fn close_dialog(&self) {
        *self.pending.borrow_mut() = None;
        *self.pending_restore.borrow_mut() = None;
        if let Some(ui) = self.ui.upgrade() {
            ui.set_dialog("".into());
        }
    }

    fn delete_loaded(&self) {
        let Some(id) = self.loaded.borrow().as_ref().map(|item| item.id.clone()) else {
            return;
        };
        let mut store = lock_store(&self.store);
        if let Err(error) = store.delete(&id) {
            drop(store);
            self.toast(&error);
            return;
        }
        drop(store);
        runtime::poke(&self.wake);
        self.log.log("INFO", &format!("删除提醒 id={id}"));
        self.show_blank();
        self.refresh_list();
        self.toast("已删除");
    }

    fn open_dialog(&self, title: &str, message: &str) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_dialog_title(title.into());
            ui.set_dialog_message(message.into());
        }
    }

    fn set_autostart(&self, on: bool) {
        match crate::autostart::set_enabled(on) {
            Ok(()) => {
                if let Some(ui) = self.ui.upgrade() {
                    ui.set_autostart_on(on);
                }
                self.toast(if on { "已开启开机自启动" } else { "已关闭开机自启动" });
            }
            Err(error) => {
                self.log.log("ERROR", &format!("开机自启动设置失败：{error}"));
                self.toast(&error);
            }
        }
    }

    fn backup(&self) {
        // 先释放存储锁，再刷新备份列表（刷新同样需要锁）。
        let result = lock_store(&self.store).backup();
        match result {
            Ok(path) => {
                self.refresh_backups();
                self.toast(&format!("已备份到 {}", path.display()));
            }
            Err(error) => self.toast(&error),
        }
    }

    fn refresh_backups(&self) {
        let files = lock_store(&self.store).list_backups();
        let labels: Vec<slint::SharedString> = files
            .iter()
            .take(BACKUP_MENU_LIMIT)
            .map(|path| backup_label(path).into())
            .collect();
        if let Some(ui) = self.ui.upgrade() {
            ui.set_backup_names(ModelRc::new(VecModel::from(labels)));
        }
        *self.backups.borrow_mut() = files;
    }

    fn ask_restore(&self, index: i32) {
        let Some(path) = usize::try_from(index)
            .ok()
            .and_then(|index| self.backups.borrow().get(index).cloned())
        else {
            return;
        };
        let label = backup_label(&path);
        *self.pending_restore.borrow_mut() = Some(path);
        self.open_dialog(
            "从这份备份恢复？",
            &format!("「{label}」会替换当前全部提醒。替换前会先自动备份当前数据。"),
        );
        if let Some(ui) = self.ui.upgrade() {
            ui.set_settings_open(false);
            ui.set_dialog("restore".into());
        }
    }

    fn restore_backup(&self) {
        let Some(path) = self.pending_restore.borrow_mut().take() else {
            return;
        };
        let result = lock_store(&self.store).restore(&path);
        match result {
            Ok(count) => {
                runtime::poke(&self.wake);
                self.log.log(
                    "INFO",
                    &format!("从备份恢复 {count} 条提醒，来源 {}", path.display()),
                );
                self.show_blank();
                self.refresh_list();
                self.refresh_backups();
                self.toast(&format!("已恢复 {count} 条提醒，恢复前的数据已另存为备份"));
            }
            Err(error) => self.toast(&error),
        }
    }

    fn preview_sound(&self) {
        let path = self.sound_path.borrow().clone();
        if let Err(error) = check_sound_file(&path) {
            if let Some(ui) = self.ui.upgrade() {
                ui.set_sound_error(error.into());
            }
            return;
        }
        match self.player.play_reminder(true, &path) {
            Ok(()) => self.toast("正在试听"),
            Err(error) => self.toast(&error),
        }
    }

    fn use_default_sound(&self) {
        *self.sound_path.borrow_mut() = String::new();
        *self.sound_cache.borrow_mut() = (String::new(), String::new());
        if let Some(ui) = self.ui.upgrade() {
            ui.set_sound_label(sound_label("").into());
            ui.set_sound_error("".into());
        }
        self.refresh_validation();
    }

    fn open_browser(&self) {
        let current = self.sound_path.borrow().clone();
        let start = PathBuf::from(&current)
            .parent()
            .filter(|path| path.is_dir())
            .map(Path::to_path_buf)
            .unwrap_or_else(home_dir);
        *self.browser_dir.borrow_mut() = start.clone();
        self.fill_dir(&start);
        if let Some(ui) = self.ui.upgrade() {
            ui.set_browser_path(if current.trim().is_empty() {
                start.display().to_string().into()
            } else {
                current.into()
            });
            ui.set_browser_picked("".into());
            ui.set_browser_open(true);
        }
    }

    fn browse_enter(&self, path: String, kind: String) {
        let file = PathBuf::from(&path);
        if kind == "dir" || file.is_dir() {
            *self.browser_dir.borrow_mut() = file.clone();
            self.fill_dir(&file);
            if let Some(ui) = self.ui.upgrade() {
                ui.set_browser_path(path.into());
                ui.set_browser_picked("".into());
            }
            return;
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_browser_picked(path.clone().into());
            ui.set_browser_path(path.into());
        }
    }

    fn browse_use(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let path = ui.get_browser_path().to_string();
        let file = PathBuf::from(path.trim());
        if file.is_dir() {
            self.browse_enter(file.display().to_string(), "dir".into());
            return;
        }
        if let Err(error) = check_sound_file(path.trim()) {
            self.toast(&error);
            return;
        }
        *self.sound_path.borrow_mut() = path.trim().to_string();
        *self.sound_cache.borrow_mut() = (String::new(), String::new());
        ui.set_sound_label(sound_label(path.trim()).into());
        ui.set_browser_open(false);
        self.refresh_validation();
    }

    fn fill_dir(&self, dir: &Path) {
        let mut entries = Vec::new();
        if let Some(parent) = dir.parent() {
            if parent != dir {
                entries.push(DirEntry {
                    label: "↑ 上一级".into(),
                    path: parent.display().to_string().into(),
                    kind: "dir".into(),
                });
            }
        }
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(read) = fs::read_dir(dir) {
            for item in read.flatten() {
                let name = item.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let path = item.path();
                if path.is_dir() {
                    dirs.push((name, path));
                } else if is_audio(&path) {
                    files.push((name, path));
                }
            }
        }
        dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        for (name, path) in dirs {
            entries.push(DirEntry {
                label: format!("[目录] {name}").into(),
                path: path.display().to_string().into(),
                kind: "dir".into(),
            });
        }
        for (name, path) in files {
            entries.push(DirEntry {
                label: name.into(),
                path: path.display().to_string().into(),
                kind: "file".into(),
            });
        }
        self.dirs.set_vec(entries);
    }

    fn refresh_list(&self) {
        self.update_tip();
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let now = Local::now();
        let search = self.search.borrow().to_lowercase();
        let filter = ui.get_list_filter().to_string();
        let store = lock_store(&self.store);
        let pending = store.reminders.iter().filter(|item| bucket(item) == "pending").count();
        let paused = store.reminders.iter().filter(|item| bucket(item) == "paused").count();
        let total = store.reminders.len();
        let today = now.date_naive();
        let today_fired = store
            .reminders
            .iter()
            .filter(|item| {
                item.last_fired_at_ms
                    .is_some_and(|ms| millis_to_local(ms).date_naive() == today)
            })
            .count();
        let due_today = store
            .reminders
            .iter()
            .filter(|item| bucket(item) == "pending")
            .filter(|item| {
                reminder_core::next_wake(std::slice::from_ref(*item), now)
                    .is_some_and(|time| time.date_naive() == today)
            })
            .count();
        ui.set_tab_all(total.to_string().into());
        ui.set_tab_pending(pending.to_string().into());
        ui.set_tab_today(due_today.to_string().into());
        ui.set_tab_fired(today_fired.to_string().into());
        ui.set_tab_paused(paused.to_string().into());
        ui.set_footer_text(format!("共 {total} 条提醒 · 待触发 {pending} 条").into());
        let mut items: Vec<&Reminder> = store
            .reminders
            .iter()
            .filter(|item| filter == "all" || bucket(item) == filter)
            .filter(|item| search.is_empty() || contains_query(item, &search))
            .collect();
        items.sort_by(|left, right| {
            rank(left)
                .cmp(&rank(right))
                .then(next_millis(left, now).cmp(&next_millis(right, now)))
                .then(left.title.cmp(&right.title))
        });
        let selected = self.selected.borrow().clone();
        let rows: Vec<ReminderRow> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let text = reminder_core::list_texts(item, now);
                ReminderRow {
                    id: item.id.clone().into(),
                    title: item.title.clone().into(),
                    summary: text.summary.into(),
                    when_text: text.when_text.into(),
                    fired_at_text: text.fired_at_text.into(),
                    scheduled_text: text.scheduled_text.into(),
                    distance_text: text.distance_text.into(),
                    badge: text.badge.into(),
                    badge_kind: text.badge_kind.into(),
                    selected: selected.as_deref() == Some(item.id.as_str()),
                    sound_on: item.sound_enabled,
                    is_last_in_group: items
                        .get(index + 1)
                        .map(|next| bucket(next) != bucket(item))
                        .unwrap_or(true),
                }
            })
            .collect();
        let visible = rows.len();
        // 概览的「今天时间线」「最近记录」也用真实数据。
        let timeline = reminder_core::today_timeline(&store.reminders, now);
        let timeline_rows: Vec<TimelineRow> = timeline
            .iter()
            .map(|entry| TimelineRow {
                time_text: entry.time_text.clone().into(),
                title: entry.title.clone().into(),
                status: entry.status.clone().into(),
                kind: entry.kind.clone().into(),
                is_next: entry.is_next,
            })
            .collect();
        let record_list: Vec<RecordRow> = reminder_core::recent_records(&store.reminders, now)
            .iter()
            .map(|entry| RecordRow {
                time_text: entry.time_text.clone().into(),
                title: entry.title.clone().into(),
                badge_text: entry.status.clone().into(),
                badge_kind: entry.kind.clone().into(),
            })
            .collect();
        drop(store);
        sync_rows(&self.rows, rows);
        ui.set_timeline(ModelRc::new(VecModel::from(timeline_rows)));
        let missed = timeline.iter().filter(|entry| entry.kind == "missed").count();
        ui.set_missed_count(missed.to_string().into());
        ui.set_records(ModelRc::new(VecModel::from(record_list)));
        ui.set_list_empty(visible == 0);
        ui.set_empty_text(if total == 0 {
            "还没有提醒。点击「新建提醒」创建第一条。"
        } else {
            "没有匹配的提醒"
        }.into());
        ui.set_count_text(visible.to_string().into());
    }

    fn show_notices(self: &Rc<Self>, mut notices: Vec<Notice>) {
        if notices.is_empty() {
            return;
        }
        let overflow = if notices.len() > 5 {
            let rest = notices.split_off(5);
            let body = rest
                .iter()
                .map(|item| format!("· {}", item.title))
                .collect::<Vec<_>>()
                .join("\n");
            Some(Notice {
                id: String::new(),
                title: format!("还有 {} 条提醒同时到期", rest.len()),
                body,
                when_text: "这些提醒已记为已触发，可在主窗口里重新编辑".into(),
                note: String::new(),
                sound_enabled: false,
                sound_path: String::new(),
                snooze: false,
                snooze_count: 0,
                kind_label: "同时到期".into(),
            })
        } else {
            None
        };
        let fired_ids: Vec<String> = notices.iter().map(|item| item.id.clone()).collect();
        for notice in notices {
            self.open_popup(notice);
        }
        if let Some(summary) = overflow {
            self.open_popup(summary);
        }
        self.refresh_list();
        let loaded_id = self.loaded.borrow().as_ref().map(|item| item.id.clone());
        if let Some(id) = loaded_id {
            if fired_ids.iter().any(|fired| fired == &id) && !self.is_dirty() {
                self.load_id(&id);
            } else if fired_ids.iter().any(|fired| fired == &id) {
                self.toast("这条提醒刚刚触发，当前编辑还没保存");
            }
        }
    }

    fn open_popup(self: &Rc<Self>, notice: Notice) {
        let popup = match PopupWindow::new() {
            Ok(popup) => popup,
            Err(error) => {
                self.log.log("ERROR", &format!("无法创建提醒弹窗：{error}"));
                return;
            }
        };
        popup.set_ui_font(ui_font().into());
        popup.set_window_title(format!("提醒 — {}", notice.title).into());
        popup.set_heading(notice.title.into());
        popup.set_body(notice.body.into());
        popup.set_when_text(notice.when_text.into());
        popup.set_note(notice.note.into());
        popup.set_is_snooze(notice.snooze);
        popup.set_kind_label(notice.kind_label.into());
        popup.set_kicker(if notice.snooze {
            format!("稍后提醒 · 第 {} 次", notice.snooze_count.saturating_add(1)).into()
        } else {
            "提醒".into()
        });
        let id = self.popup_seq.get();
        self.popup_seq.set(id + 1);
        let index = self.popups.borrow().len() as u32;
        // 右上角「还有 N 条」：当前弹窗栈里还叠着几条。
        let stack = i32::try_from(self.popups.borrow().len()).unwrap_or(0) + 1;
        popup.set_stack_count(stack);
        for slot in self.popups.borrow().iter() {
            slot.window.set_stack_count(stack);
        }

        let dismiss_popups = self.popups.clone();
        popup.on_dismiss(move || dismiss_popup(&dismiss_popups, id, true));
        let popups = self.popups.clone();
        let session_for_snooze = self.clone();
        let reminder_id = notice.id.clone();
        popup.on_snooze({
            let session = session_for_snooze.clone();
            let id = id;
            let popups = popups.clone();
            let reminder_id = reminder_id.clone();
            move |minutes| {
                session.apply_snooze(&reminder_id, u32::try_from(minutes).unwrap_or(0));
                dismiss_popup(&popups, id, true);
            }
        });
        popup.on_snooze_tomorrow({
            let session = session_for_snooze.clone();
            let id = id;
            let popups = popups.clone();
            let reminder_id = reminder_id.clone();
            move || {
                let now = Local::now();
                let minutes = u32::try_from(minutes_until(now, 9, 0) + if now.hour() < 9 { 0 } else { 24 * 60 })
                    .unwrap_or(5)
                    .max(1);
                session.apply_snooze(&reminder_id, minutes);
                dismiss_popup(&popups, id, true);
            }
        });
        popup.on_show_next({
            let popups = popups.clone();
            move || {
                let slots = popups.borrow();
                let mut ordered: Vec<&PopupSlot> = slots.iter().filter(|slot| slot.id != id).collect();
                ordered.sort_by_key(|slot| slot.id);
                if let Some(next) = ordered.first() {
                    let _ = next.window.show();
                    focus_window(next.window.window());
                }
            }
        });
        let popups = self.popups.clone();
        popup.window().on_close_requested(move || {
            dismiss_popup(&popups, id, false);
            slint::CloseRequestResponse::HideWindow
        });
        let session = self.clone();
        let target = notice.id.clone();
        popup.on_open_main(move || session.open_reminder(&target));

        if let Err(error) = popup.show() {
            self.log.log("ERROR", &format!("无法显示提醒弹窗：{error}"));
            return;
        }
        place_window(popup.window(), index, 440, 540, true, true);
        let weak = popup.as_weak();
        let mut timers = Vec::new();
        for delay in [80u64, 280] {
            let timer = slint::Timer::default();
            let weak = weak.clone();
            timer.start(slint::TimerMode::SingleShot, Duration::from_millis(delay), move || {
                if let Some(popup) = weak.upgrade() {
                    place_window(popup.window(), index, 440, 540, true, true);
                }
            });
            timers.push(timer);
        }
        if let Err(error) = self.player.play_reminder(notice.sound_enabled, &notice.sound_path) {
            self.log.log("WARN", &format!("铃声未播放：{error}"));
        }
        self.popups.borrow_mut().push(PopupSlot { id, window: popup, _timers: timers });
    }

    fn hide_main(&self) {
        if let Some(ui) = self.ui.upgrade() {
            let _ = ui.hide();
        }
        self.update_tip();
    }

    /// 托盘左键：窗口可见则隐藏，否则显示并置前。
    fn toggle_main(&self) {
        let visible = self.ui.upgrade().is_some_and(|ui| ui.window().is_visible());
        if visible {
            self.hide_main();
        } else {
            self.show_main();
        }
    }

    /// 显示并置前主窗口；已显示时只置前。
    fn show_main(&self) {
        self.reveal_main();
        self.close_tray_menu();
    }

    fn reveal_main(&self) {
        if let Some(ui) = self.ui.upgrade() {
            let _ = ui.show();
            focus_window(ui.window());
            self.refresh_list();
        }
    }

    /// 弹窗里的「打开主窗口」：显示主窗口并定位到这条提醒。
    fn open_reminder(&self, id: &str) {
        self.reveal_main();
        if !id.is_empty() {
            self.request_nav(Nav::Select(id.to_string()));
        }
    }

    fn new_reminder_from_tray(&self) {
        self.reveal_main();
        self.request_nav(Nav::New);
    }

    fn backup_from_tray(&self) {
        self.reveal_main();
        self.backup();
    }

    /// 托盘「退出」与顶栏一致：有待触发提醒时先二次确认。
    fn quit_from_tray(&self) {
        let pending = lock_store(&self.store)
            .reminders
            .iter()
            .any(|item| bucket(item) == "pending");
        if !pending {
            self.quit();
            return;
        }
        self.reveal_main();
        self.open_dialog("退出桌面提醒？", "还有待触发的提醒，退出后将不会再按时提醒。");
        if let Some(ui) = self.ui.upgrade() {
            ui.set_dialog("quit".into());
        }
    }

    fn quit(&self) {
        self.log.log("INFO", "用户退出");
        let _ = slint::quit_event_loop();
    }

    /// 托盘提示：有稍后提醒时显示下次再提醒的时间，否则显示后台运行状态。
    fn update_tip(&self) {
        let has_snooze = self.snooze_pending();
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.set_tip(self.tip_text().into());
        }
        if let Some(tx) = self.tray_icon.borrow().as_ref() {
            let _ = tx.send(xembed_tray::TrayIconMsg::Badge(has_snooze));
            let _ = tx.send(xembed_tray::TrayIconMsg::Tooltip(self.tip_text()));
        }
    }

    fn snooze_pending(&self) -> bool {
        let now = Local::now().timestamp_millis();
        let store = lock_store(&self.store);
        store
            .reminders
            .iter()
            .any(|item| item.snooze_until_ms.map(|ms| ms > now).unwrap_or(false))
    }

    fn tip_text(&self) -> String {
        let now = Local::now().timestamp_millis();
        let store = lock_store(&self.store);
        let next_snooze = store
            .reminders
            .iter()
            .filter_map(|item| item.snooze_until_ms)
            .filter(|ms| *ms > now)
            .min();
        match next_snooze {
            Some(ms) => format!("稍后提醒将于 {} 再次提醒", millis_to_local(ms).format("%H:%M")),
            None => "桌面提醒 · 后台运行中".to_string(),
        }
    }

    /// 托盘菜单的两行状态：待触发数量，以及下一条提醒。
    fn tray_status(&self) -> (String, String) {
        let now = Local::now();
        let store = lock_store(&self.store);
        let pending = store
            .reminders
            .iter()
            .filter(|item| bucket(item) == "pending")
            .count();
        let status = if pending == 0 {
            "后台运行中 · 暂无待触发".to_string()
        } else {
            format!("后台运行中 · {pending} 条待触发")
        };
        let next: Option<(String, DateTime<Local>)> = store
            .reminders
            .iter()            .filter(|item| bucket(item) == "pending")
            .filter_map(|item| {
                reminder_core::next_wake(std::slice::from_ref(item), now)
                    .map(|time| (item.title.clone(), time))
            })
            .min_by_key(|(_, time)| *time);
        let next_text = match next {
            Some((title, time)) => format!("下次：{title} · {}", time.format("%H:%M")),
            None => "下次：暂无提醒".to_string(),
        };
        (status, next_text)
    }

    /// 设置入口：托盘菜单里的「设置…」。先叫回主窗口再展开设置浮层。
    fn open_settings(&self) {
        self.reveal_main();
        if let Some(ui) = self.ui.upgrade() {
            ui.set_settings_open(true);
        }
    }

    /// 稍后提醒：改写触发时间并落盘，成功后刷新列表并提示。
    fn apply_snooze(&self, reminder_id: &str, minutes: u32) {
        let mut store = lock_store(&self.store);
        let outcome = store
            .reminders
            .iter_mut()
            .find(|item| item.id == reminder_id)
            .map(|reminder| arm_snooze(reminder, minutes, Local::now()));
        match outcome {
            Some(Ok(())) => {
                if let Err(error) = store.save() {
                    self.log.log("ERROR", &format!("稍后提醒保存失败：{error}"));
                }
                drop(store);
                runtime::poke(&self.wake);
                self.log.log("INFO", &format!("稍后提醒 id={reminder_id} minutes={minutes}"));
                if let Some(ui) = self.ui.upgrade() {
                    ui.set_status_text(format!("将在 {minutes} 分钟后再次提醒").into());
                }
                self.refresh_list();
            }
            Some(Err(error)) => self.log.log("WARN", &error),
            None => self.log.log("WARN", "稍后提醒时找不到这条提醒"),
        }
    }

    fn minimize_main(&self) {
        if let Some(ui) = self.ui.upgrade() {
            let _ = with_winit(ui.window(), |winit_window| {
                winit_window.set_minimized(true);
            });
        }
    }

    fn maximize_main(&self) {
        if let Some(ui) = self.ui.upgrade() {
            let _ = with_winit(ui.window(), |winit_window| {
                let maximized = winit_window.is_maximized();
                winit_window.set_maximized(!maximized);
            });
        }
    }

    /// 侧栏快速添加：点选时间即创建，标题留空时给个默认名。
    fn quick_create(&self, title: String, preset: i32) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let now = Local::now();
        let title = {
            let trimmed = title.trim();
            if trimmed.is_empty() {
                "快速提醒".to_string()
            } else {
                trimmed.to_string()
            }
        };
        let minutes = match preset {
            2000 => minutes_until(now, 20, 0),
            900 => minutes_until(now, 9, 0) + if now.hour() < 9 { 0 } else { 24 * 60 },
            other => i64::try_from(other).unwrap_or(1),
        }
        .max(1);
        let mut draft = Draft::blank();
        draft.title = title.clone();
        draft.kind = Kind::Delay;
        draft.minutes = minutes.to_string();
        let mut store = lock_store(&self.store);
        if let Some(reason) = store.persist_block_reason().map(str::to_string) {
            drop(store);
            self.toast(&reason);
            return;
        }
        let count = store.reminders.len();
        let reminder = match build_reminder_mapped(&draft, None, now, count) {
            Ok(reminder) => reminder,
            Err(report) => {
                drop(store);
                show_report(&ui, &report);
                self.toast("快速创建没有成功，请检查表单");
                return;
            }
        };
        if let Err(error) = store.upsert(reminder.clone()) {
            drop(store);
            self.toast(&error);
            return;
        }
        drop(store);
        runtime::poke(&self.wake);
        self.refresh_list();
        self.log.log("INFO", &format!("快速创建 id={} minutes={minutes}", reminder.id));
    }

    /// 指定时间页的快捷时间：0=今晚 20:00，1=今晚 21:00，2=明早 9:00，3=下周一 9:00。
    fn at_preset(&self, index: i32) {
        let now = Local::now();
        let (date, time) = match index {
            0 => (today_date(now), "20:00".to_string()),
            1 => (today_date(now), "21:00".to_string()),
            2 => (tomorrow_date(now), "09:00".to_string()),
            _ => (next_monday_date(now), "09:00".to_string()),
        };
        if let Some(ui) = self.ui.upgrade() {
            ui.set_kind("at".into());
            ui.set_form_date(date.into());
            ui.set_form_time(time.into());
        }
        self.reveal_errors.set(true);
        self.refresh_validation();
    }

    /// 「每次提醒的时间」改了以后重算 Cron 前两段，日/月/周保持原样。
    fn cron_time_changed(&self, text: String) {
        let trimmed = text.trim().to_string();
        let parsed = trimmed.split_once(':').and_then(|(hour, minute)| {
            let hour: u32 = hour.trim().parse().ok()?;
            let minute: u32 = minute.trim().parse().ok()?;
            if hour > 23 || minute > 59 {
                return None;
            }
            Some((hour, minute))
        });
        let Some((hour, minute)) = parsed else {
            self.toast("时间要用 HH:MM 的格式，例如 09:00");
            return;
        };
        let current = self.read_draft().cron.trim().to_string();
        let mut parts: Vec<String> = if current.is_empty() {
            vec!["0".into(), "9".into(), "*".into(), "*".into(), "*".into()]
        } else {
            current.split_whitespace().map(str::to_string).collect()
        };
        while parts.len() < 5 {
            parts.push("*".into());
        }
        parts[0] = minute.to_string();
        parts[1] = hour.to_string();
        let expression = parts.join(" ");
        if let Some(ui) = self.ui.upgrade() {
            ui.set_kind("cron".into());
            ui.set_form_cron(expression.clone().into());
            ui.set_form_time(format!("{hour:02}:{minute:02}").into());
        }
        self.reveal_errors.set(true);
        self.refresh_validation();
        self.log.log("INFO", &format!("Cron 时间调整为 {expression}"));
    }

    /// 「预览弹窗」：用当前草稿拼一条提醒，直接弹一次看效果。
    fn preview_popup(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let draft = self.read_draft();
        let now = Local::now();
        let reminder = match build_reminder_mapped(&draft, None, now, 0) {
            Ok(reminder) => reminder,
            Err(_) => {
                self.toast("表单还有错误，先修正后再预览");
                return;
            }
        };
        let texts = reminder_core::list_texts(&reminder, now);
        let when = match reminder.schedule {
            reminder_core::Schedule::Delay { .. } | reminder_core::Schedule::At { .. } => {
                reminder_core::next_wake(std::slice::from_ref(&reminder), now)
                    .map(|time| reminder_core::format_point(time, now, true))
                    .unwrap_or_else(|| texts.when_text.clone())
            }
            reminder_core::Schedule::Cron { .. } => texts.when_text.clone(),
        };
        self.open_popup(Notice {
            id: reminder.id.clone(),
            title: reminder.title.clone(),
            body: if reminder.body.trim().is_empty() {
                "（没有正文）".to_string()
            } else {
                reminder.body.clone()
            },
            when_text: when,
            note: "这是预览，不会改动已保存的提醒。".to_string(),
            kind_label: texts.badge.clone(),
            snooze: false,
            snooze_count: 0,
            sound_enabled: false,
            sound_path: String::new(),
        });
        ui.set_status_text("预览弹窗".into());
    }

    fn open_tray_menu(self: &Rc<Self>, anchor: xembed_tray::TrayMenuAnchor) {
        self.close_tray_menu();
        TRAY_POPUP.with(|flag| flag.set(true));
        let menu = match TrayMenu::new() {
            Ok(menu) => menu,
            Err(error) => {
                TRAY_POPUP.with(|flag| flag.set(false));
                self.log.log("ERROR", &format!("无法打开托盘菜单：{error}"));
                self.show_main();
                return;
            }
        };
        TRAY_POPUP.with(|flag| flag.set(false));
        menu.set_ui_font(ui_font().into());
        let (status, next) = self.tray_status();
        menu.set_status_text(status.into());
        menu.set_next_text(next.into());
        let session = self.clone();
        menu.on_show_main({
            let session = session.clone();
            move || session.reveal_main()
        });
        menu.on_new_reminder({
            let session = session.clone();
            move || session.new_reminder_from_tray()
        });
        menu.on_backup({
            let session = session.clone();
            move || session.backup_from_tray()
        });
        menu.on_open_settings({
            let session = session.clone();
            move || session.open_settings()
        });
        menu.on_quit_app({
            let session = session.clone();
            move || session.quit_from_tray()
        });
        let weak = menu.as_weak();
        menu.on_dismiss(move || {
            if let Some(menu) = weak.upgrade() {
                let _ = menu.hide();
            }
        });
        // Root coordinates and the tray anchor are physical pixels. Ask winit for
        // the scale factor of the actual anchor monitor; the main window may be on
        // a different display with a different DPI. RandR's physical-DPI estimate
        // remains a fallback if winit cannot match the monitor.
        let fallback_scale = anchor.monitor_scale_milli.max(1000) as f64 / 1000.0;
        let scale = self
            .ui
            .upgrade()
            .and_then(|ui| {
                with_winit(ui.window(), |window| {
                    let icon_x = anchor.icon_x.saturating_add(anchor.icon_w / 2);
                    let icon_y = anchor.icon_y.saturating_add(anchor.icon_h / 2);
                    window.available_monitors().find_map(|monitor| {
                        let origin = monitor.position();
                        let size = monitor.size();
                        let right = i64::from(origin.x) + i64::from(size.width);
                        let bottom = i64::from(origin.y) + i64::from(size.height);
                        (i64::from(icon_x) >= i64::from(origin.x)
                            && i64::from(icon_x) < right
                            && i64::from(icon_y) >= i64::from(origin.y)
                            && i64::from(icon_y) < bottom)
                            .then(|| monitor.scale_factor())
                    })
                })
            })
            .flatten()
            .unwrap_or(fallback_scale)
            .max(1.0) as f32;
        let menu_w = (xembed_tray::MENU_W as f32 * scale).round() as i32;
        let menu_h = (xembed_tray::MENU_H as f32 * scale).round() as i32;
        menu.window().set_size(slint::PhysicalSize::new(menu_w as u32, menu_h as u32));
        let (x, y) = xembed_tray::tray_menu_origin(&anchor, menu_w, menu_h);
        let position = slint::PhysicalPosition::new(x, y);
        menu.window().set_position(position);
        if menu.show().is_err() {
            return;
        }
        let placed = menu.as_weak();
        slint::Timer::single_shot(Duration::from_millis(0), move || {
            if let Some(menu) = placed.upgrade() {
                menu.window().set_position(position);
            }
        });
        *self.menu.borrow_mut() = Some(menu);
    }

    fn close_tray_menu(&self) {
        if let Some(menu) = self.menu.borrow_mut().take() {
            let _ = menu.hide();
        }
    }

    fn poll_show_request(&self) {
        let flag = self.data_dir.join("show.request");
        if flag.exists() {
            let _ = fs::remove_file(&flag);
            self.show_main();
            self.log.log("INFO", "收到再次打开主窗口的请求");
        }
    }

    fn tick_clock(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let now = Local::now();
        ui.set_clock_text(now.format("%H:%M").to_string().into());
        ui.set_date_text(design_date_text(now).into());
        let offset = now.offset().fix().local_minus_utc();
        let sign = if offset >= 0 { '+' } else { '-' };
        let offset = offset.abs();
        ui.set_zone_text(format!("UTC{sign}{:02}:{:02}", offset / 3600, (offset % 3600) / 60).into());
    }

    fn toast(&self, message: &str) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_status_text(message.into());
        }
        self.log.log("INFO", message);
    }
}

fn dismiss_popup(popups: &Rc<RefCell<Vec<PopupSlot>>>, id: u64, hide: bool) {
    let weak = {
        let slots = popups.borrow();
        slots.iter().find(|slot| slot.id == id).map(|slot| slot.window.as_weak())
    };
    let popups = popups.clone();
    slint::Timer::single_shot(Duration::from_millis(0), move || {
        if hide {
            if let Some(popup) = weak.and_then(|window| window.upgrade()) {
                let _ = popup.hide();
            }
        }
        popups.borrow_mut().retain(|slot| slot.id != id);
    });
}

fn show_report(ui: &MainWindow, report: &Report) {
    ui.set_title_error(field(report, "title").into());
    ui.set_body_error(field(report, "body").into());
    ui.set_delay_error(field(report, "delay").into());
    ui.set_date_error(field(report, "date").into());
    ui.set_time_error(field(report, "time").into());
    ui.set_cron_error(field(report, "cron").into());
    ui.set_sound_error(field(report, "sound").into());
    ui.set_form_error(field(report, "form").into());
    ui.set_delay_hint(report.delay_hint.clone().into());
    ui.set_at_hint(report.at_hint.clone().into());
    let lines: Vec<PreviewLine> = report
        .preview
        .iter()
        .map(|text| PreviewLine { text: text.clone().into() })
        .collect();
    ui.set_preview_lines(ModelRc::from(Rc::new(VecModel::from(lines))));
    let next = if report.preview.is_empty() {
        String::new()
    } else {
        let shown: Vec<&str> = report.preview.iter().take(4).map(String::as_str).collect();
        format!("接下来：{}", shown.join(" · "))
    };
    ui.set_preview_next(next.into());
}

fn field(report: &Report, name: &str) -> String {
    report
        .errors
        .iter()
        .find(|error| error.field == name)
        .map(|error| error.message.clone())
        .unwrap_or_default()
}

fn banner_text(loaded: Option<&Reminder>, draft: &Draft) -> String {
    if loaded.map(|item| item.status) == Some(Status::Fired) {
        if draft.enabled {
            "这条提醒已经触发过。保存后会按下方设置重新启用。".into()
        } else {
            "这条提醒已经触发过。当前未勾选启用，保存后只更新内容，不会开始计时。".into()
        }
    } else if !draft.enabled && loaded.is_some() {
        match draft.kind {
            Kind::Delay => "已暂停。重新勾选并保存后，会从当前时间重新计时。".into(),
            Kind::At => "已暂停。重新勾选并保存后，会按指定时间触发。".into(),
            Kind::Cron => "已暂停。重新勾选并保存后，从下一周期继续。".into(),
        }
    } else {
        String::new()
    }
}

fn history_text(loaded: Option<&Reminder>) -> String {
    let Some(ms) = loaded.and_then(|item| item.last_fired_at_ms) else {
        return String::new();
    };
    format!("上次触发：{}", format_point(millis_to_local(ms), Local::now(), true))
}

fn build_reminder_mapped(
    draft: &Draft,
    previous: Option<&Reminder>,
    now: chrono::DateTime<Local>,
    count: usize,
) -> Result<Reminder, Report> {
    reminder_core::build_reminder(draft, previous, now, count)
}

const BACKUP_MENU_LIMIT: usize = 5;

/// 备份文件名形如 reminders-20260101-120000123.json，显示时转成可读时间。
fn backup_label(path: &Path) -> String {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let stamp = name.trim_start_matches("reminders-").trim_end_matches(".json");
    let parsed = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S%3f")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S"));
    match parsed {
        Ok(at) => at.format("%Y-%m-%d %H:%M:%S").to_string(),
        Err(_) => name.to_string(),
    }
}

fn lock_store(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|error| error.into_inner())
}

fn sync_rows(model: &VecModel<ReminderRow>, rows: Vec<ReminderRow>) {
    let same = model.row_count() == rows.len()
        && model.iter().zip(rows.iter()).all(|(left, right)| left.id == right.id);
    if same {
        for (index, row) in rows.into_iter().enumerate() {
            model.set_row_data(index, row);
        }
    } else {
        model.set_vec(rows);
    }
}

fn contains_query(reminder: &Reminder, query: &str) -> bool {
    reminder.title.to_lowercase().contains(query)
        || reminder.body.to_lowercase().contains(query)
        || match &reminder.schedule {
            reminder_core::Schedule::Cron { expression } => expression.to_lowercase().contains(query),
            _ => false,
        }
}

fn bucket(reminder: &Reminder) -> &'static str {
    if reminder.status == Status::Fired {
        "fired"
    } else if !reminder.enabled {
        "paused"
    } else {
        "pending"
    }
}

fn rank(reminder: &Reminder) -> u8 {
    if reminder.status == Status::Fired {
        2
    } else if !reminder.enabled {
        1
    } else {
        0
    }
}

fn next_millis(reminder: &Reminder, now: chrono::DateTime<Local>) -> i64 {
    reminder_core::next_wake(std::slice::from_ref(reminder), now)
        .map(|time| time.timestamp_millis())
        .unwrap_or(i64::MAX)
}

/// 现在到今天（或调用方补一天后）h:m 还有多少分钟。
fn minutes_until(now: DateTime<Local>, hour: u32, minute: u32) -> i64 {
    use chrono::TimeZone as _;
    let naive = match now.date_naive().and_hms_opt(hour, minute, 0) {
        Some(naive) => naive,
        None => return 60,
    };
    let target = match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(value) | chrono::LocalResult::Ambiguous(value, _) => value,
        chrono::LocalResult::None => {
            let shifted = naive + chrono::Duration::hours(1);
            match Local.from_local_datetime(&shifted) {
                chrono::LocalResult::Single(value) | chrono::LocalResult::Ambiguous(value, _) => value,
                chrono::LocalResult::None => return 60,
            }
        }
    };
    (target - now).num_minutes().max(1)
}

fn design_date_text(now: DateTime<Local>) -> String {
    let weekday = match now.weekday().num_days_from_monday() {
        0 => "周一",
        1 => "周二",
        2 => "周三",
        3 => "周四",
        4 => "周五",
        5 => "周六",
        _ => "周日",
    };
    format!("{} 月 {} 日 · {weekday}", now.month(), now.day())
}

fn today_date(now: DateTime<Local>) -> String {
    now.format("%Y-%m-%d").to_string()
}

fn tomorrow_date(now: DateTime<Local>) -> String {
    (now + chrono::Duration::days(1)).format("%Y-%m-%d").to_string()
}

fn next_monday_date(now: DateTime<Local>) -> String {
    use chrono::Datelike as _;
    let mut day = now + chrono::Duration::days(1);
    while day.weekday().num_days_from_monday() != 0 {
        day += chrono::Duration::days(1);
    }
    day.format("%Y-%m-%d").to_string()
}

fn sound_label(path: &str) -> String {
    if path.trim().is_empty() {
        "默认铃声（内置）".into()
    } else {
        path.trim().to_string()
    }
}

fn is_audio(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()).as_deref(),
        Some("wav" | "mp3" | "ogg" | "flac")
    )
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn ui_font() -> &'static str {
    if cfg!(windows) {
        "Microsoft YaHei UI"
    } else if cfg!(target_os = "macos") {
        "PingFang SC"
    } else {
        "Noto Sans CJK SC"
    }
}

fn mono_font() -> &'static str {
    // 设计稿：数字与 Cron 用 JetBrains Mono，回退 DejaVu Sans Mono。
    if cfg!(windows) {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "JetBrains Mono"
    }
}

fn focus_window(window: &slint::Window) {
    let _ = with_winit(window, |winit_window| {
        winit_window.focus_window();
    });
}

fn place_window(
    window: &slint::Window,
    stack: u32,
    logical_w: u32,
    logical_h: u32,
    on_top: bool,
    prefer_cursor: bool,
) {
    let _ = with_winit(window, |winit_window| {
        winit_window.set_window_level(if on_top {
            winit::window::WindowLevel::AlwaysOnTop
        } else {
            winit::window::WindowLevel::Normal
        });
        if on_top {
            let _ = winit_window.request_user_attention(Some(winit::window::UserAttentionType::Critical));
        }
        // 提醒弹窗优先放到光标所在的显示器；Wayland 没有全局光标坐标（cursor_point 返回 None），
        // 主窗口本身则固定跟随系统主屏，不受光标移动影响。
        let monitors: Vec<_> = winit_window
            .available_monitors()
            .map(|monitor| (monitor.position(), monitor.size(), monitor.scale_factor()))
            .collect();
        let cursor = if prefer_cursor { cursor_point() } else { None };
        let on_cursor = cursor.and_then(|(cursor_x, cursor_y)| {
            monitors.iter().copied().find(|(origin, size, _)| {
                cursor_x >= origin.x
                    && cursor_x < origin.x + size.width as i32
                    && cursor_y >= origin.y
                    && cursor_y < origin.y + size.height as i32
            })
        });
        let (origin, size, scale) = if let Some(hit) = on_cursor {
            hit
        } else if let Some(monitor) = winit_window.primary_monitor() {
            (monitor.position(), monitor.size(), monitor.scale_factor())
        } else if let Some(monitor) = winit_window.current_monitor() {
            (monitor.position(), monitor.size(), monitor.scale_factor())
        } else if let Some(fallback) = monitors.first().copied() {
            fallback
        } else {
            return;
        };
        let outer = winit_window.outer_size();
        let (win_w, win_h) = if outer.width > 80 && outer.height > 80 {
            (outer.width, outer.height)
        } else {
            (
                (logical_w as f64 * scale) as u32 + 16,
                (logical_h as f64 * scale) as u32 + 48,
            )
        };
        let (x, y) = reminder_core::center_on_monitor(
            origin.x,
            origin.y,
            size.width,
            size.height,
            win_w,
            win_h,
            stack,
        );
        winit_window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    });
}

/// 纯 X11 会话下向 X 服务器查询全局光标位置（物理像素）。
/// 纯 Wayland 会话拿不到全局光标坐标，返回 None，由调用方回退到主显示器。
#[cfg(all(unix, not(target_os = "macos")))]
fn cursor_point() -> Option<(i32, i32)> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return None;
    }
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt as _;
    let (connection, screen_num) = x11rb::connect(None).ok()?;
    let screen = connection.setup().roots.get(screen_num)?;
    let reply = connection.query_pointer(screen.root).ok()?.reply().ok()?;
    Some((reply.root_x as i32, reply.root_y as i32))
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn cursor_point() -> Option<(i32, i32)> {
    None
}

fn with_winit<T>(window: &slint::Window, f: impl FnOnce(&winit::window::Window) -> T) -> Option<T> {
    use i_slint_backend_winit::WinitWindowAccessor;
    window.with_winit_window(f)
}

fn place_main(window: &slint::Window) {
    place_window(window, 0, 1100, 740, false, false);
}

fn select_backend() -> Result<(), String> {
    let backend = i_slint_backend_winit::Backend::builder()
        .with_renderer_name("renderer-software")
        .with_window_attributes_hook(tray_popup_attributes)
        .build()
        .map_err(|error| format!("无法初始化窗口：{error}"))?;
    slint::platform::set_platform(Box::new(backend)).map_err(|error| format!("无法初始化窗口：{error}"))
}

fn tray_popup_attributes(attributes: winit::window::WindowAttributes) -> winit::window::WindowAttributes {
    if !TRAY_POPUP.with(Cell::get) {
        return attributes;
    }
    TRAY_POPUP.with(|flag| flag.set(false));
    popup_window(attributes)
}

fn popup_window(attributes: winit::window::WindowAttributes) -> winit::window::WindowAttributes {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use winit::platform::x11::{WindowAttributesExtX11, WindowType};
        return attributes
            .with_override_redirect(true)
            .with_x11_window_type(vec![WindowType::PopupMenu])
            .with_decorations(false);
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    attributes
}
