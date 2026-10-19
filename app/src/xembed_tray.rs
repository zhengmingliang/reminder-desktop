//! Linux Mint Cinnamon 的面板托盘是 XEmbed（`_NET_SYSTEM_TRAY_S0`），不提供
//! `org.kde.StatusNotifierWatcher`。Slint 自带的托盘走的是后者，所以在 Mint 上没有图标。
//!
//! 托盘选择窗口本身经常是深度 0。用这个深度去 PutImage 会被 X 拒绝，窗口上就只剩创建时
//! 涂上的纯色块。Cinnamon 公布的 32 位视觉嵌进去后又会被合成成近乎纯黑的方块，带 SHAPE
//! 的窗口则停在未映射的插槽里，面板上根本不出现。
//!
//! 这里用根窗口的视觉把铃铛画进图标，背景取屏幕底边最常见的颜色，也就是面板本身的颜色。
//! 方块和面板融在一起，露出来的是铃铛。

use std::sync::mpsc::{Receiver, Sender};
use std::sync::OnceLock;
use std::time::Duration;
use std::thread;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt, CreateGCAux,
    CreateWindowAux, EventMask, ImageFormat, PropMode, WindowClass, CLIENT_MESSAGE_EVENT,
};
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as XrandrConnectionExt;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub enum TrayCommand {
    Show,
    Menu(TrayMenuAnchor),
}

/// 外部（界面）发给托盘线程的消息：切换「有稍后提醒」角标、更新悬浮提示。
#[derive(Clone, Debug)]
pub enum TrayIconMsg {
    Badge(bool),
    /// 更新面板悬浮提示（XEmbed 用 _NET_WM_NAME 当 tooltip）。
    Tooltip(String),
}

/// 右键时图标在根窗口上的位置。菜单要在窗口创建前就算好，不能等窗口出现再挪。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayMenuAnchor {
    pub pointer_x: i32,
    pub pointer_y: i32,
    pub icon_x: i32,
    pub icon_y: i32,
    pub icon_w: i32,
    pub icon_h: i32,
    pub screen_w: i32,
    pub screen_h: i32,
    /// Bounds of the monitor containing the icon in virtual-desktop coordinates.
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub monitor_w: i32,
    pub monitor_h: i32,
    pub monitor_scale_milli: u32,
}

const ICON: u16 = 22;

/// 注册 XEmbed 托盘图标，返回向托盘线程发消息的发送端（用于更新角标）。
pub fn spawn(tx: Sender<TrayCommand>) -> Result<Sender<TrayIconMsg>, String> {
    let (icon_tx, icon_rx) = std::sync::mpsc::channel::<TrayIconMsg>();
    if !tray_present() {
        return Err("当前会话没有 XEmbed 系统托盘".into());
    }
    thread::Builder::new()
        .name("reminder-tray".into())
        .spawn(move || {
            if let Err(error) = tray_loop(tx, icon_rx) {
                eprintln!("托盘图标退出：{error}");
            }
        })
        .map_err(|error| format!("无法启动托盘线程：{error}"))?;
    Ok(icon_tx)
}

fn tray_present() -> bool {
    let Ok((conn, screen)) = RustConnection::connect(None) else {
        return false;
    };
    tray_owner(&conn, screen).unwrap_or(0) != 0
}

fn tray_owner(conn: &RustConnection, screen: usize) -> Result<u32, String> {
    let name = format!("_NET_SYSTEM_TRAY_S{screen}");
    let atom = intern(conn, &name)?;
    let owner = conn
        .get_selection_owner(atom)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .owner;
    Ok(owner)
}

fn tray_loop(tx: Sender<TrayCommand>, rx: Receiver<TrayIconMsg>) -> Result<(), String> {
    let (conn, screen_num) = RustConnection::connect(None).map_err(|error| error.to_string())?;
    let mut icon = dock(&conn, screen_num)?;
    loop {
        // 先处理界面发来的消息（角标开关），再看有没有 X 事件。
        for msg in rx.try_iter() {
            match msg {
                TrayIconMsg::Badge(on) => {
                    if icon.badge != on {
                        icon.badge = on;
                        if let Err(error) = paint(&conn, &mut icon) {
                            eprintln!("托盘图标绘制失败：{error}");
                        }
                    }
                }
                TrayIconMsg::Tooltip(text) => {
                    if let Err(error) = set_net_wm_name(&conn, icon.window, &text) {
                        eprintln!("托盘提示更新失败：{error}");
                    }
                }
            }
        }
        let Some(event) = conn.poll_for_event().map_err(|error| error.to_string())? else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        match event {
            Event::Expose(expose) if expose.window == icon.window && expose.count == 0 => {
                if let Err(error) = paint(&conn, &mut icon) {
                    eprintln!("托盘图标绘制失败：{error}");
                }
            }
            Event::ConfigureNotify(cfg) if cfg.window == icon.window => {
                icon.width = cfg.width.max(1);
                icon.height = cfg.height.max(1);
                if let Err(error) = paint(&conn, &mut icon) {
                    eprintln!("托盘图标绘制失败：{error}");
                }
            }
            Event::ReparentNotify(reparent) if reparent.window == icon.window => {
                if let Err(error) = paint(&conn, &mut icon) {
                    eprintln!("托盘图标绘制失败：{error}");
                }
            }
            Event::ButtonRelease(button) if button.event == icon.window && button.detail == 1 => {
                let _ = tx.send(TrayCommand::Show);
            }
            Event::ButtonRelease(button) if button.event == icon.window && button.detail == 3 => {
                let anchor = menu_anchor(
                    &conn,
                    screen_num,
                    icon.window,
                    i32::from(button.root_x),
                    i32::from(button.root_y),
                );
                let _ = tx.send(TrayCommand::Menu(anchor));
            }
            Event::DestroyNotify(destroyed)
                if destroyed.window == icon.window || destroyed.window == icon.tray =>
            {
                free_pixmap(&conn, &icon);
                if let Ok(next) = dock(&conn, screen_num) {
                    icon = next;
                }
            }
            _ => {}
        }
    }
}

struct IconWindow {
    window: u32,
    tray: u32,
    gc: u32,
    pixmap: u32,
    pixmap_w: u16,
    pixmap_h: u16,
    width: u16,
    height: u16,
    depth: u8,
    red: u32,
    green: u32,
    blue: u32,
    bg: (u8, u8, u8),
    /// 是否有稍后提醒在等待：有则在图标右上角画橙色角标。
    badge: bool,
}

struct TrayVisual {
    id: u32,
    depth: u8,
    red: u32,
    green: u32,
    blue: u32,
}

fn dock(conn: &RustConnection, screen_num: usize) -> Result<IconWindow, String> {
    let tray = tray_owner(conn, screen_num)?;
    if tray == 0 {
        return Err("系统托盘不在".into());
    }
    let bg = panel_color(conn, screen_num);
    let screen = &conn.setup().roots[screen_num];
    let visual = screen_visual(screen);
    let background = pack(bg.0, bg.1, bg.2, 255, visual.red, visual.green, visual.blue, 0);
    let window = conn.generate_id().map_err(|error| error.to_string())?;
    checked(
        conn.create_window(
            visual.depth,
            window,
            screen.root,
            0,
            0,
            ICON,
            ICON,
            0,
            WindowClass::INPUT_OUTPUT,
            visual.id,
            &CreateWindowAux::new()
                .background_pixel(background)
                .border_pixel(0)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::STRUCTURE_NOTIFY,
                ),
        ),
    )?;
    let xembed = intern(conn, "_XEMBED_INFO")?;
    checked(conn.change_property32(PropMode::REPLACE, window, xembed, xembed, &[0, 1]))?;
    let utf8 = intern(conn, "UTF8_STRING")?;
    let net_name = intern(conn, "_NET_WM_NAME")?;
    checked(conn.change_property8(
        PropMode::REPLACE,
        window,
        net_name,
        utf8,
        "桌面提醒".as_bytes(),
    ))?;
    checked(conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"reminder-desktop",
    ))?;
    // 面板的悬浮提示读 _NET_WM_NAME（UTF8），设计稿要求「桌面提醒 · 后台运行中」。
    set_net_wm_name(conn, window, "桌面提醒 · 后台运行中")?;
    let gc = conn.generate_id().map_err(|error| error.to_string())?;
    checked(conn.create_gc(gc, window, &CreateGCAux::new()))?;
    checked(conn.map_window(window))?;
    let mut icon = IconWindow {
        window,
        tray,
        gc,
        pixmap: 0,
        pixmap_w: 0,
        pixmap_h: 0,
        width: ICON,
        height: ICON,
        depth: visual.depth,
        red: visual.red,
        green: visual.green,
        blue: visual.blue,
        bg,
        badge: false,
    };
    paint(conn, &mut icon)?;
    let opcode = intern(conn, "_NET_SYSTEM_TRAY_OPCODE")?;
    let message = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window: tray,
        type_: opcode,
        data: [x11rb::CURRENT_TIME, 0, window, 0, 0].into(),
    };
    checked(conn.send_event(false, tray, EventMask::NO_EVENT, message))?;
    conn.flush().map_err(|error| error.to_string())?;
    Ok(icon)
}

fn screen_visual(screen: &x11rb::protocol::xproto::Screen) -> TrayVisual {
    for depth in &screen.allowed_depths {
        for visual in &depth.visuals {
            if visual.visual_id == screen.root_visual {
                return TrayVisual {
                    id: screen.root_visual,
                    depth: depth.depth,
                    red: visual.red_mask,
                    green: visual.green_mask,
                    blue: visual.blue_mask,
                };
            }
        }
    }
    TrayVisual {
        id: screen.root_visual,
        depth: screen.root_depth,
        red: 0x00ff_0000,
        green: 0x0000_ff00,
        blue: 0x0000_00ff,
    }
}

fn panel_color(conn: &RustConnection, screen_num: usize) -> (u8, u8, u8) {
    let (root, width, height) = {
        let screen = &conn.setup().roots[screen_num];
        (screen.root, screen.width_in_pixels, screen.height_in_pixels)
    };
    let y = i16::try_from(height.saturating_sub(1)).unwrap_or(0);
    let Ok(cookie) = conn.get_image(ImageFormat::Z_PIXMAP, root, 0, y, width.max(1), 1, u32::MAX) else {
        return (28, 28, 32);
    };
    let Ok(image) = cookie.reply() else {
        return (28, 28, 32);
    };
    let mut counts = std::collections::HashMap::<(u8, u8, u8), u32>::new();
    let mut index = 0;
    while index + 2 < image.data.len() {
        let color = (image.data[index + 2], image.data[index + 1], image.data[index]);
        *counts.entry(color).or_insert(0) += 1;
        index += 4;
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(color, _)| color)
        .unwrap_or((28, 28, 32))
}

fn paint(conn: &RustConnection, icon: &mut IconWindow) -> Result<(), String> {
    let width = icon.width.max(1);
    let height = icon.height.max(1);
    icon.width = width;
    icon.height = height;
    let pixels = tray_image(icon);
    if icon.pixmap == 0 || icon.pixmap_w != width || icon.pixmap_h != height {
        let pixmap = conn.generate_id().map_err(|error| error.to_string())?;
        checked(conn.create_pixmap(icon.depth, pixmap, icon.window, width, height))?;
        checked(conn.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            icon.gc,
            width,
            height,
            0,
            0,
            0,
            icon.depth,
            &pixels,
        ))?;
        // 先换成新背景，再释放旧的。托盘清窗口时画的就是这张图。
        checked(conn.change_window_attributes(
            icon.window,
            &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
        ))?;
        if icon.pixmap != 0 {
            let _ = conn.free_pixmap(icon.pixmap);
        }
        icon.pixmap = pixmap;
        icon.pixmap_w = width;
        icon.pixmap_h = height;
    } else {
        checked(conn.put_image(
            ImageFormat::Z_PIXMAP,
            icon.pixmap,
            icon.gc,
            width,
            height,
            0,
            0,
            0,
            icon.depth,
            &pixels,
        ))?;
    }
    checked(conn.clear_area(false, icon.window, 0, 0, 0, 0))?;
    checked(conn.put_image(
        ImageFormat::Z_PIXMAP,
        icon.window,
        icon.gc,
        width,
        height,
        0,
        0,
        0,
        icon.depth,
        &pixels,
    ))?;
    conn.flush().map_err(|error| error.to_string())?;
    Ok(())
}

fn free_pixmap(conn: &RustConnection, icon: &IconWindow) {
    if icon.pixmap != 0 {
        let _ = conn.free_pixmap(icon.pixmap);
    }
}

fn menu_anchor(
    conn: &RustConnection,
    screen_num: usize,
    window: u32,
    event_x: i32,
    event_y: i32,
) -> TrayMenuAnchor {
    let screen = &conn.setup().roots[screen_num];
    let (icon_x, icon_y, icon_w, icon_h) = icon_rect(conn, screen.root, window);
    let monitors = randr_monitors(conn, screen.root, screen.width_in_pixels, screen.height_in_pixels);
    let mut anchor = TrayMenuAnchor {
        pointer_x: event_x,
        pointer_y: event_y,
        icon_x,
        icon_y,
        icon_w,
        icon_h,
        screen_w: i32::from(screen.width_in_pixels),
        screen_h: i32::from(screen.height_in_pixels),
        monitor_x: 0,
        monitor_y: 0,
        monitor_w: 0,
        monitor_h: 0,
        monitor_scale_milli: 1000,
    };
    anchor = anchor_on_monitor(anchor, &monitors);
    if anchor.monitor_w <= 0 || anchor.monitor_h <= 0 {
        anchor.monitor_w = i32::from(screen.width_in_pixels);
        anchor.monitor_h = i32::from(screen.height_in_pixels);
    }
    let (pointer_x, pointer_y) = conn
        .query_pointer(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .filter(|pointer| pointer.same_screen)
        .map(|pointer| (i32::from(pointer.root_x), i32::from(pointer.root_y)))
        .unwrap_or((event_x, event_y));
    anchor.pointer_x = pointer_x;
    anchor.pointer_y = pointer_y;
    anchor
}

fn randr_monitors(
    conn: &RustConnection,
    root: u32,
    screen_w: u16,
    screen_h: u16,
) -> Vec<(i32, i32, i32, i32, u32)> {
    let monitors = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| {
            reply
                .monitors
                .into_iter()
                .filter(|monitor| monitor.width > 0 && monitor.height > 0)
                .map(|monitor| {
                    (
                        i32::from(monitor.x),
                        i32::from(monitor.y),
                        i32::from(monitor.width),
                        i32::from(monitor.height),
                        monitor_scale_milli(
                            monitor.width,
                            monitor.height,
                            monitor.width_in_millimeters,
                            monitor.height_in_millimeters,
                        ),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if monitors.is_empty() {
        return vec![(0, 0, i32::from(screen_w), i32::from(screen_h), 1000)];
    }
    let screen_bounds = (0, 0, i32::from(screen_w), i32::from(screen_h));
    let mut monitors: Vec<_> = monitors
        .into_iter()
        .filter_map(|(x, y, w, h, scale)| {
            let left = x.max(screen_bounds.0);
            let top = y.max(screen_bounds.1);
            let right = x.saturating_add(w).min(screen_bounds.0 + screen_bounds.2);
            let bottom = y.saturating_add(h).min(screen_bounds.1 + screen_bounds.3);
            (right > left && bottom > top).then_some((left, top, right - left, bottom - top, scale))
        })
        .collect();
    if monitors.is_empty() {
        vec![screen_bounds_with_scale(screen_w, screen_h)]
    } else {
        monitors.sort_by_key(|(x, y, _, _, _)| (*y, *x));
        monitors
    }
}

fn screen_bounds_with_scale(screen_w: u16, screen_h: u16) -> (i32, i32, i32, i32, u32) {
    (0, 0, i32::from(screen_w), i32::from(screen_h), 1000)
}

fn monitor_scale_milli(width: u16, height: u16, width_mm: u32, height_mm: u32) -> u32 {
    if width_mm == 0 || height_mm == 0 {
        return 1000;
    }
    let pixels_per_mm = (f64::from(width) * f64::from(height)
        / (f64::from(width_mm) * f64::from(height_mm)))
        .sqrt();
    let scale = ((pixels_per_mm * (12.0 * 25.4 / 96.0)).round() / 12.0).max(1.0);
    if scale.is_finite() && scale <= 20.0 {
        (scale * 1000.0).round() as u32
    } else {
        1000
    }
}

fn icon_rect(conn: &RustConnection, root: u32, window: u32) -> (i32, i32, i32, i32) {
    let Ok(cookie) = conn.get_geometry(window) else {
        return (0, 0, 0, 0);
    };
    let Ok(geom) = cookie.reply() else {
        return (0, 0, 0, 0);
    };
    let Ok(cookie) = conn.translate_coordinates(window, root, 0, 0) else {
        return (0, 0, 0, 0);
    };
    let Ok(translated) = cookie.reply() else {
        return (0, 0, 0, 0);
    };
    (
        i32::from(translated.dst_x),
        i32::from(translated.dst_y),
        i32::from(geom.width),
        i32::from(geom.height),
    )
}

/// 托盘菜单的逻辑尺寸，和设计稿一致（264 宽）。菜单窗口和定位共用这两个值。
pub const MENU_W: i32 = 264;
pub const MENU_H: i32 = 292;

/// Select the monitor containing the tray icon, in virtual-desktop physical coordinates.
pub fn anchor_on_monitor(
    mut anchor: TrayMenuAnchor,
    monitors: &[(i32, i32, i32, i32, u32)],
) -> TrayMenuAnchor {
    let (point_x, point_y) = if anchor.icon_w > 0 && anchor.icon_h > 0 {
        (
            anchor.icon_x.saturating_add(anchor.icon_w / 2),
            anchor.icon_y.saturating_add(anchor.icon_h / 2),
        )
    } else {
        (anchor.pointer_x, anchor.pointer_y)
    };
    if let Some(&(x, y, w, h, scale_milli)) = monitors.iter().find(|&&(x, y, w, h, _)| {
        w > 0 && h > 0 && point_x >= x && point_x < x.saturating_add(w)
            && point_y >= y && point_y < y.saturating_add(h)
    }) {
        anchor.monitor_x = x;
        anchor.monitor_y = y;
        anchor.monitor_w = w;
        anchor.monitor_h = h;
        anchor.monitor_scale_milli = scale_milli.max(1);
    }
    anchor
}

/// Position the menu above the icon on its own monitor; only use the virtual root as fallback.
pub fn tray_menu_origin(anchor: &TrayMenuAnchor, menu_w: i32, menu_h: i32) -> (i32, i32) {
    let gap = 6;
    let (bounds_x, bounds_y, bounds_w, bounds_h) = if anchor.monitor_w > 0 && anchor.monitor_h > 0 {
        (anchor.monitor_x, anchor.monitor_y, anchor.monitor_w, anchor.monitor_h)
    } else {
        (0, 0, anchor.screen_w, anchor.screen_h)
    };
    let (mut x, mut y) = if anchor.icon_w > 0 && anchor.icon_h > 0 {
        let right = anchor.icon_x.saturating_add(anchor.icon_w);
        let above = anchor.icon_y.saturating_sub(menu_h).saturating_sub(gap);
        let below = anchor.icon_y.saturating_add(anchor.icon_h).saturating_add(gap);
        let bottom = bounds_y.saturating_add(bounds_h);
        let y = if above >= bounds_y {
            above
        } else if below.saturating_add(menu_h) <= bottom {
            below
        } else if anchor.icon_y.saturating_sub(bounds_y) >= bottom.saturating_sub(anchor.icon_y) {
            above
        } else {
            below
        };
        (right.saturating_sub(menu_w), y)
    } else {
        let above = anchor.pointer_y.saturating_sub(menu_h).saturating_sub(gap);
        let below = anchor.pointer_y.saturating_add(gap);
        let bottom = bounds_y.saturating_add(bounds_h);
        let y = if above >= bounds_y { above } else if below.saturating_add(menu_h) <= bottom { below } else { above };
        (anchor.pointer_x, y)
    };
    if bounds_w > 0 {
        let max_x = bounds_x.saturating_add(bounds_w).saturating_sub(menu_w).max(bounds_x);
        x = x.clamp(bounds_x, max_x);
    } else {
        x = x.max(bounds_x);
    }
    if bounds_h > 0 {
        let max_y = bounds_y.saturating_add(bounds_h).saturating_sub(menu_h).max(bounds_y);
        y = y.clamp(bounds_y, max_y);
    } else {
        y = y.max(bounds_y);
    }
    (x, y)
}

/// 设计稿里的托盘图标底色：深青色 #0B6B63。
const TILE_TEAL: (u8, u8, u8) = (0x0B, 0x6B, 0x63);
/// 铃铛占圆角方块的比例，四周留出呼吸空间。
const BELL_SCALE: f32 = 0.72;

/// 圆角方块：角落之外露出面板底色，其余区域都是青色底。
fn tile_shape(nx: f32, ny: f32) -> bool {
    const R: f32 = 0.24;
    let dx = nx - nx.clamp(R, 1.0 - R);
    let dy = ny - ny.clamp(R, 1.0 - R);
    dx * dx + dy * dy <= R * R
}

/// 把方块坐标映射到铃铛坐标后判断是否落在白色铃铛上。
fn bell_glyph(nx: f32, ny: f32) -> bool {
    bell_shape(
        (nx - 0.5) / BELL_SCALE + 0.5,
        (ny - 0.5) / BELL_SCALE + 0.5,
    )
}

/// 托盘图标原图：设计稿第 10 页的规格，128×128 的深青圆角方块 + 白色铃铛。
const TRAY_PNG: &[u8] = include_bytes!("../assets/tray/icon-128.png");

fn tray_source() -> Result<&'static image::RgbaImage, String> {
    static SOURCE: OnceLock<image::RgbaImage> = OnceLock::new();
    if let Some(loaded) = SOURCE.get() {
        return Ok(loaded);
    }
    let decoded = image::load_from_memory(TRAY_PNG).map_err(|error| error.to_string())?;
    let rgba = decoded.to_rgba8();
    Ok(SOURCE.get_or_init(|| rgba))
}

/// 把设计稿图标缩放到托盘窗口尺寸，叠在面板底色上；有稍后提醒时补橙色角标。
fn tray_image(icon: &IconWindow) -> Vec<u8> {
    let width = icon.width.max(1);
    let height = icon.height.max(1);
    let Ok(source) = tray_source() else {
        return bell_image(width, height, icon.red, icon.green, icon.blue, icon.bg);
    };
    let scaled = image::imageops::resize(
        source,
        u32::from(width),
        u32::from(height),
        image::imageops::FilterType::Lanczos3,
    );
    // 设计稿：角标直径约为图标的 38%，外面一圈 2px 面板底色描边。
    let radius = (f32::from(width) * 0.19).max(2.5);
    let ring = radius + (f32::from(width) * 0.08).max(1.0);
    let cx = f32::from(width) - ring - 0.5;
    let cy = ring - 0.5;
    let badge = (247, 144, 9); // #F79009
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let px = scaled.get_pixel(u32::from(x), u32::from(y)).0;
            let a = f32::from(px[3]) / 255.0;
            let (mut r, mut g, mut b) = (
                (f32::from(px[0]) * a + f32::from(icon.bg.0) * (1.0 - a)).round() as u8,
                (f32::from(px[1]) * a + f32::from(icon.bg.1) * (1.0 - a)).round() as u8,
                (f32::from(px[2]) * a + f32::from(icon.bg.2) * (1.0 - a)).round() as u8,
            );
            if icon.badge {
                let dx = f32::from(x) + 0.5 - cx;
                let dy = f32::from(y) + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= radius {
                    (r, g, b) = badge;
                } else if dist <= ring {
                    (r, g, b) = icon.bg;
                }
            }
            data.extend_from_slice(&pack(r, g, b, 255, icon.red, icon.green, icon.blue, 0).to_ne_bytes());
        }
    }
    data
}

fn bell_image(width: u16, height: u16, red: u32, green: u32, blue: u32, bg: (u8, u8, u8)) -> Vec<u8> {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let nx = (f32::from(x) + 0.5) / f32::from(width);
            let ny = (f32::from(y) + 0.5) / f32::from(height);
            let (r, g, b) = if !tile_shape(nx, ny) {
                bg
            } else if bell_shape(nx, ny) {
                (255, 255, 255)
            } else {
                TILE_TEAL
            };
            data.extend_from_slice(&pack(r, g, b, 255, red, green, blue, 0).to_ne_bytes());
        }
    }
    data
}

fn bell_shape(nx: f32, ny: f32) -> bool {
    let cx = nx - 0.5;
    let knob = cx * cx + (ny - 0.16) * (ny - 0.16) <= 0.055 * 0.055;
    let body = if (0.22..0.64).contains(&ny) {
        let t = (ny - 0.22) / 0.42;
        cx.abs() <= 0.08 + t * 0.28
    } else {
        false
    };
    let lip = (0.60..0.72).contains(&ny) && cx.abs() <= 0.40;
    let clapper = cx * cx + (ny - 0.83) * (ny - 0.83) <= 0.065 * 0.065;
    knob || body || lip || clapper
}

fn pack(r: u8, g: u8, b: u8, a: u8, red: u32, green: u32, blue: u32, alpha: u32) -> u32 {
    channel(r, red) | channel(g, green) | channel(b, blue) | channel(a, alpha)
}

fn channel(value: u8, mask: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    (u32::from(value) * (mask >> mask.trailing_zeros()).min(255) / 255) << mask.trailing_zeros()
}

/// 面板把 _NET_WM_NAME 当托盘悬停提示，用 UTF8_STRING 写中文。
fn set_net_wm_name(conn: &RustConnection, window: u32, text: &str) -> Result<(), String> {
    let name = intern(conn, "_NET_WM_NAME")?;
    let utf8 = intern(conn, "UTF8_STRING")?;
    checked(conn.change_property8(
        PropMode::REPLACE,
        window,
        name,
        utf8,
        text.as_bytes(),
    ))?;
    conn.flush().map_err(|error| error.to_string())
}

fn checked(
    cookie: Result<x11rb::cookie::VoidCookie<'_, RustConnection>, x11rb::errors::ConnectionError>,
) -> Result<(), String> {
    cookie
        .map_err(|error| error.to_string())?
        .check()
        .map_err(|error| error.to_string())
}

fn intern(conn: &RustConnection, name: &str) -> Result<u32, String> {
    conn.intern_atom(false, name.as_bytes())
        .map_err(|error| error.to_string())?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        anchor_on_monitor, bell_image, bell_shape, dock, free_pixmap, monitor_scale_milli, paint,
        tray_menu_origin, tile_shape,
        TrayMenuAnchor, MENU_H, MENU_W,
    };
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;

    #[test]
    fn icon_is_a_teal_tile_with_a_white_bell() {
        let bg = (28, 28, 32);
        let data = bell_image(24, 24, 0x00ff_0000, 0x0000_ff00, 0x0000_00ff, bg);
        let mut white = 0;
        let mut teal = 0;
        let mut panel = 0;
        for px in data.chunks(4) {
            let word = u32::from_ne_bytes([px[0], px[1], px[2], px[3]]);
            let color = (
                ((word >> 16) & 0xff) as u8,
                ((word >> 8) & 0xff) as u8,
                (word & 0xff) as u8,
            );
            if color == (255, 255, 255) {
                white += 1;
            } else if color == (0x0B, 0x6B, 0x63) {
                teal += 1;
            } else if color == bg {
                panel += 1;
            }
        }
        assert!(white > 40, "white {white}");
        assert!(teal > white, "teal {teal} white {white}");
        assert!(panel > 0 && panel < 24 * 24 / 4, "panel {panel}");
        assert!(!tile_shape(0.02, 0.02));
        assert!(tile_shape(0.5, 0.02));
        assert!(bell_shape(0.5, 0.4));
        assert!(bell_shape(0.5, 0.83));
    }

    #[test]
    fn menu_opens_above_a_bottom_right_icon() {
        let anchor = TrayMenuAnchor {
            pointer_x: 3790,
            pointer_y: 1580,
            icon_x: 3779,
            icon_y: 1568,
            icon_w: 24,
            icon_h: 24,
            screen_w: 4480,
            screen_h: 1600,
            monitor_x: 1920,
            monitor_y: 0,
            monitor_w: 2560,
            monitor_h: 1600,
            monitor_scale_milli: 1000,
        };
        let (x, y) = tray_menu_origin(&anchor, MENU_W, MENU_H);
        assert!(x > 3000, "x={x}");
        assert!(x >= 1920 && x + MENU_W <= 4480, "x={x}");
        assert!(y >= 0 && y + MENU_H <= 1568, "y={y}");
        assert!(y + MENU_H <= 1600);
    }

    /// 会在当前桌面的托盘上短暂放一个铃铛。普通 `cargo test` 不跑。
    /// `REMINDER_TRAY_PREVIEW=1 cargo test -p reminder-desktop -- docked_icon -- --ignored --test-threads=1`
    #[test]
    #[ignore]
    fn docked_icon_uses_a_bell_not_a_flat_square() {
        if std::env::var_os("REMINDER_TRAY_PREVIEW").is_none() {
            return;
        }
        let (conn, screen_num) = RustConnection::connect(None).expect("display");
        let mut icon = dock(&conn, screen_num).expect("dock");
        let screen_h = conn.setup().roots[screen_num].height_in_pixels;
        let bottom = i16::try_from(screen_h.saturating_sub(160)).unwrap_or(0);
        let start = std::time::Instant::now();
        let mut placed = None;
        while start.elapsed().as_millis() < 2_000 {
            while let Some(event) = conn.poll_for_event().unwrap() {
                match event {
                    Event::Expose(expose) if expose.window == icon.window && expose.count == 0 => {
                        paint(&conn, &mut icon).unwrap();
                    }
                    Event::ConfigureNotify(cfg) if cfg.window == icon.window => {
                        icon.width = cfg.width.max(1);
                        icon.height = cfg.height.max(1);
                        paint(&conn, &mut icon).unwrap();
                    }
                    Event::ReparentNotify(reparent) if reparent.window == icon.window => {
                        paint(&conn, &mut icon).unwrap();
                    }
                    _ => {}
                }
            }
            let root = conn.setup().roots[screen_num].root;
            if let Ok(cookie) = conn.translate_coordinates(icon.window, root, 0, 0) {
                if let Ok(at) = cookie.reply() {
                    if at.dst_y >= bottom {
                        placed = Some((at.dst_x, at.dst_y));
                        break;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let root = conn.setup().roots[screen_num].root;
        let Some((x, y)) = placed else {
            let _ = conn.destroy_window(icon.window);
            let _ = conn.flush();
            panic!("icon stayed off the panel, bg {:?}", icon.bg);
        };
        paint(&conn, &mut icon).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let image = conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                root,
                x,
                y,
                icon.width,
                icon.height,
                u32::MAX,
            )
            .unwrap()
            .reply()
            .expect("icon image");
        let stride = image.data.len() / usize::from(icon.height.max(1));
        let bg_luma = u32::from(icon.bg.0) + u32::from(icon.bg.1) + u32::from(icon.bg.2);
        let mut bright = 0usize;
        let mut ppm = format!("P6\n{} {}\n255\n", icon.width, icon.height).into_bytes();
        for row in 0..usize::from(icon.height) {
            for col in 0..usize::from(icon.width) {
                let i = row * stride + col * 4;
                let (b, g, r) = if i + 2 < image.data.len() {
                    (image.data[i], image.data[i + 1], image.data[i + 2])
                } else {
                    (0, 0, 0)
                };
                let luma = u32::from(r) + u32::from(g) + u32::from(b);
                if luma > bg_luma + 120 {
                    bright += 1;
                }
                ppm.extend_from_slice(&[r, g, b]);
            }
        }
        let total = usize::from(icon.width) * usize::from(icon.height);
        let report = format!(
            "window {:#x} {}x{} at {x},{y} depth {} bg {:?} bright {bright}/{total}",
            icon.window, icon.width, icon.height, icon.depth, icon.bg
        );
        let _ = std::fs::write("/tmp/tray-bell-preview.txt", &report);
        let _ = std::fs::write("/tmp/tray-bell-preview.ppm", &ppm);
        free_pixmap(&conn, &icon);
        let _ = conn.destroy_window(icon.window);
        let _ = conn.flush();
        assert!(
            bright >= 40 && bright * 10 < total * 8,
            "icon is not a bell: {report}"
        );
    }

    #[test]
    fn menu_stays_on_screen_when_the_icon_is_at_the_top_left() {
        let anchor = TrayMenuAnchor {
            pointer_x: 8,
            pointer_y: 8,
            icon_x: 0,
            icon_y: 0,
            icon_w: 24,
            icon_h: 24,
            screen_w: 1920,
            screen_h: 1080,
            monitor_x: 0,
            monitor_y: 0,
            monitor_w: 1920,
            monitor_h: 1080,
            monitor_scale_milli: 1000,
        };
        let (x, y) = tray_menu_origin(&anchor, MENU_W, MENU_H);
        assert_eq!((x, y), (0, 30));
    }

    #[test]
    fn menu_uses_the_monitor_that_contains_the_tray_icon() {
        let anchor = TrayMenuAnchor {
            pointer_x: 1898,
            pointer_y: 1050,
            icon_x: 1888,
            icon_y: 1040,
            icon_w: 24,
            icon_h: 24,
            screen_w: 4480,
            screen_h: 1600,
            monitor_x: 0,
            monitor_y: 0,
            monitor_w: 0,
            monitor_h: 0,
            monitor_scale_milli: 1000,
        };
        let monitors = [
            (0, 0, 1920, 1080, 1000),
            (1920, 0, 2560, 1600, 2000),
        ];
        let anchor = anchor_on_monitor(anchor, &monitors);
        assert_eq!((anchor.monitor_x, anchor.monitor_y), (0, 0));
        assert_eq!(anchor.monitor_scale_milli, 1000);
        let (x, y) = tray_menu_origin(&anchor, MENU_W, MENU_H);
        assert!(x >= 0 && x + MENU_W <= 1920);
        assert!(y >= 0 && y + MENU_H <= 1080);

        let laptop_anchor = TrayMenuAnchor {
            pointer_x: 4000,
            pointer_y: 1500,
            icon_x: 4400,
            icon_y: 1550,
            icon_w: 24,
            icon_h: 24,
            screen_w: 4480,
            screen_h: 1600,
            monitor_x: 0,
            monitor_y: 0,
            monitor_w: 0,
            monitor_h: 0,
            monitor_scale_milli: 1000,
        };
        let laptop_anchor = anchor_on_monitor(laptop_anchor, &monitors);
        assert_eq!(laptop_anchor.monitor_x, 1920);
        assert_eq!(laptop_anchor.monitor_scale_milli, 2000);
        let scale = laptop_anchor.monitor_scale_milli as f32 / 1000.0;
        let (x, y) = tray_menu_origin(
            &laptop_anchor,
            (MENU_W as f32 * scale).round() as i32,
            (MENU_H as f32 * scale).round() as i32,
        );
        assert!(x >= 1920 && x + 528 <= 4480);
        assert!(y >= 0 && y + 584 <= 1600);
    }

    #[test]
    fn monitor_scale_matches_winit_randr_dpi_quantization() {
        assert_eq!(monitor_scale_milli(1920, 1080, 527, 296), 1000);
        assert_eq!(monitor_scale_milli(2560, 1600, 345, 215), 2000);
        assert_eq!(monitor_scale_milli(100, 100, 0, 0), 1000);
    }
}
