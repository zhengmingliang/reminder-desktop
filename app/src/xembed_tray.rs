//! Linux Mint Cinnamon 的面板托盘是 XEmbed（`_NET_SYSTEM_TRAY_S0`），不提供
//! `org.kde.StatusNotifierWatcher`。Slint 自带的托盘走的是后者，所以在 Mint 上没有图标。
//!
//! 托盘选择窗口本身经常是深度 0。用这个深度去 PutImage 会被 X 拒绝，窗口上就只剩创建时
//! 涂上的纯色块。Cinnamon 公布的 32 位视觉嵌进去后又会被合成成近乎纯黑的方块，带 SHAPE
//! 的窗口则停在未映射的插槽里，面板上根本不出现。
//!
//! 这里用根窗口的视觉把铃铛画进图标，背景取屏幕底边最常见的颜色，也就是面板本身的颜色。
//! 方块和面板融在一起，露出来的是铃铛。

use std::sync::mpsc::Sender;
use std::thread;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt, CreateGCAux,
    CreateWindowAux, EventMask, ImageFormat, PropMode, WindowClass, CLIENT_MESSAGE_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub enum TrayCommand {
    Show,
    Menu(TrayMenuAnchor),
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
}

const ICON: u16 = 22;

pub fn spawn(tx: Sender<TrayCommand>) -> Result<(), String> {
    if !tray_present() {
        return Err("当前会话没有 XEmbed 系统托盘".into());
    }
    thread::Builder::new()
        .name("reminder-tray".into())
        .spawn(move || {
            if let Err(error) = tray_loop(tx) {
                eprintln!("托盘图标退出：{error}");
            }
        })
        .map_err(|error| format!("无法启动托盘线程：{error}"))?;
    Ok(())
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

fn tray_loop(tx: Sender<TrayCommand>) -> Result<(), String> {
    let (conn, screen_num) = RustConnection::connect(None).map_err(|error| error.to_string())?;
    let mut icon = dock(&conn, screen_num)?;
    loop {
        let event = conn.wait_for_event().map_err(|error| error.to_string())?;
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
        b"reminder-tray",
    ))?;
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
    let pixels = bell_image(width, height, icon.red, icon.green, icon.blue, icon.bg);
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
    let (pointer_x, pointer_y) = conn
        .query_pointer(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .filter(|pointer| pointer.same_screen)
        .map(|pointer| (i32::from(pointer.root_x), i32::from(pointer.root_y)))
        .unwrap_or((event_x, event_y));
    TrayMenuAnchor {
        pointer_x,
        pointer_y,
        icon_x,
        icon_y,
        icon_w,
        icon_h,
        screen_w: i32::from(screen.width_in_pixels),
        screen_h: i32::from(screen.height_in_pixels),
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

/// 菜单贴着图标上沿，右边缘和图标对齐，并留在虚拟屏幕里。
pub fn tray_menu_origin(anchor: &TrayMenuAnchor, menu_w: i32, menu_h: i32) -> (i32, i32) {
    let gap = 6;
    let (mut x, mut y) = if anchor.icon_w > 0 && anchor.icon_h > 0 {
        let right = anchor.icon_x.saturating_add(anchor.icon_w);
        let above = anchor.icon_y.saturating_sub(menu_h).saturating_sub(gap);
        let below = anchor
            .icon_y
            .saturating_add(anchor.icon_h)
            .saturating_add(gap);
        let y = if above >= 0 { above } else { below };
        (right.saturating_sub(menu_w), y)
    } else {
        let above = anchor.pointer_y.saturating_sub(menu_h).saturating_sub(gap);
        let y = if above >= 0 {
            above
        } else {
            anchor.pointer_y.saturating_add(gap)
        };
        (anchor.pointer_x, y)
    };
    if anchor.screen_w > 0 {
        let max_x = anchor.screen_w.saturating_sub(menu_w).max(0);
        x = x.clamp(0, max_x);
    } else if x < 0 {
        x = 0;
    }
    if anchor.screen_h > 0 {
        let max_y = anchor.screen_h.saturating_sub(menu_h).max(0);
        y = y.clamp(0, max_y);
    } else if y < 0 {
        y = 0;
    }
    (x, y)
}

fn bell_image(width: u16, height: u16, red: u32, green: u32, blue: u32, bg: (u8, u8, u8)) -> Vec<u8> {
    let glyph = glyph_color(bg);
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let nx = (f32::from(x) + 0.5) / f32::from(width);
            let ny = (f32::from(y) + 0.5) / f32::from(height);
            let (_, _, _, alpha) = bell_pixel(nx, ny);
            let (r, g, b) = if alpha == 0 { bg } else { glyph };
            data.extend_from_slice(&pack(r, g, b, 255, red, green, blue, 0).to_ne_bytes());
        }
    }
    data
}

fn glyph_color(bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let luma = u32::from(bg.0) * 3 + u32::from(bg.1) * 6 + u32::from(bg.2);
    if luma > 1_600 {
        (32, 37, 41)
    } else {
        (255, 255, 255)
    }
}

fn bell_pixel(nx: f32, ny: f32) -> (u8, u8, u8, u8) {
    if bell_shape(nx, ny) {
        return (255, 255, 255, 255);
    }
    let outline = 0.045;
    let neighbors = [
        (nx - outline, ny),
        (nx + outline, ny),
        (nx, ny - outline),
        (nx, ny + outline),
    ];
    if neighbors.into_iter().any(|(x, y)| bell_shape(x, y)) {
        return (32, 37, 41, 230);
    }
    (0, 0, 0, 0)
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
        bell_image, bell_pixel, bell_shape, dock, free_pixmap, paint, tray_menu_origin,
        TrayMenuAnchor,
    };
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;

    #[test]
    fn bell_is_a_glyph_not_a_filled_square() {
        let mut opaque = 0;
        let mut picture = String::new();
        for y in 0..24 {
            for x in 0..24 {
                let nx = (x as f32 + 0.5) / 24.0;
                let ny = (y as f32 + 0.5) / 24.0;
                let (_, _, _, alpha) = bell_pixel(nx, ny);
                if alpha > 0 {
                    opaque += 1;
                }
                picture.push(if bell_shape(nx, ny) {
                    '#'
                } else if alpha > 0 {
                    '+'
                } else {
                    '.'
                });
            }
            picture.push('\n');
        }
        assert!(opaque > 40, "{picture}");
        assert!(opaque < 24 * 24 / 2, "{picture}");
        assert_eq!(bell_pixel(0.02, 0.02).3, 0, "{picture}");
        assert!(bell_shape(0.5, 0.4), "{picture}");
        assert!(bell_shape(0.5, 0.83), "{picture}");
    }

    #[test]
    fn bell_image_keeps_panel_color_outside_the_glyph() {
        let bg = (28, 28, 32);
        let data = bell_image(24, 24, 0x00ff_0000, 0x0000_ff00, 0x0000_00ff, bg);
        let mut glyph = 0;
        let mut panel = 0;
        for px in data.chunks(4) {
            let word = u32::from_ne_bytes([px[0], px[1], px[2], px[3]]);
            let color = (
                ((word >> 16) & 0xff) as u8,
                ((word >> 8) & 0xff) as u8,
                (word & 0xff) as u8,
            );
            if color == (255, 255, 255) {
                glyph += 1;
            } else if color == bg {
                panel += 1;
            }
        }
        assert!(glyph > 40, "glyph {glyph}");
        assert!(panel > 24 * 24 / 2, "panel {panel}");
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
        };
        let (x, y) = tray_menu_origin(&anchor, 196, 84);
        assert!(x > 3000, "x={x}");
        assert!(y > 1400 && y < 1568, "y={y}");
        assert!(x + 196 <= 4480);
        assert!(y + 84 <= 1600);
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
        };
        let (x, y) = tray_menu_origin(&anchor, 196, 84);
        assert_eq!((x, y), (0, 30));
    }
}
