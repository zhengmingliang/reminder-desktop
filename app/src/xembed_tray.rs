//! Linux Mint Cinnamon 的面板托盘是 XEmbed（`_NET_SYSTEM_TRAY_S0`），不提供
//! `org.kde.StatusNotifierWatcher`。Slint 自带的托盘走的是后者，所以在 Mint 上没有图标。

use std::sync::mpsc::Sender;
use std::thread;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ColormapAlloc, ConfigWindow,
    ConfigureWindowAux, ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, ImageFormat,
    PropMode, WindowClass, CLIENT_MESSAGE_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub enum TrayCommand {
    Show,
    Menu { x: i32, y: i32 },
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
            Event::Expose(expose) if expose.window == icon.window => {
                paint(&conn, &icon)?;
            }
            Event::ConfigureNotify(cfg) if cfg.window == icon.window => {
                icon.width = cfg.width;
                icon.height = cfg.height;
                paint(&conn, &icon)?;
            }
            Event::ButtonRelease(button) if button.event == icon.window && button.detail == 1 => {
                let _ = tx.send(TrayCommand::Show);
            }
            Event::ButtonRelease(button) if button.event == icon.window && button.detail == 3 => {
                let _ = tx.send(TrayCommand::Menu {
                    x: i32::from(button.root_x),
                    y: i32::from(button.root_y),
                });
            }
            Event::DestroyNotify(destroyed)
                if destroyed.window == icon.window || destroyed.window == icon.tray =>
            {
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
    width: u16,
    height: u16,
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
    let screen = &conn.setup().roots[screen_num];
    let tray_geom = conn
        .get_geometry(tray)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?;
    let attrs = conn
        .get_window_attributes(tray)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?;
    let (red, green, blue) = visual_masks(conn, screen_num, attrs.visual);
    let window = conn.generate_id().map_err(|error| error.to_string())?;
    let colormap = conn.generate_id().map_err(|error| error.to_string())?;
    conn.create_colormap(ColormapAlloc::NONE, colormap, screen.root, attrs.visual)
        .map_err(|error| error.to_string())?;
    conn.create_window(
        tray_geom.depth,
        window,
        screen.root,
        0,
        0,
        ICON,
        ICON,
        0,
        WindowClass::INPUT_OUTPUT,
        attrs.visual,
        &CreateWindowAux::new()
            .colormap(colormap)
            .background_pixel(pack(11, 110, 107, red, green, blue))
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::STRUCTURE_NOTIFY,
            ),
    )
    .map_err(|error| error.to_string())?;
    let xembed = intern(conn, "_XEMBED_INFO")?;
    conn.change_property32(PropMode::REPLACE, window, xembed, xembed, &[0, 1])
        .map_err(|error| error.to_string())?;
    let utf8 = intern(conn, "UTF8_STRING")?;
    let net_name = intern(conn, "_NET_WM_NAME")?;
    conn.change_property8(
        PropMode::REPLACE,
        window,
        net_name,
        utf8,
        "桌面提醒".as_bytes(),
    )
    .map_err(|error| error.to_string())?;
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"reminder-tray",
    )
    .map_err(|error| error.to_string())?;
    let gc = conn.generate_id().map_err(|error| error.to_string())?;
    conn.create_gc(gc, window, &CreateGCAux::new())
        .map_err(|error| error.to_string())?;
    conn.map_window(window).map_err(|error| error.to_string())?;
    let opcode = intern(conn, "_NET_SYSTEM_TRAY_OPCODE")?;
    let message = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window: tray,
        type_: opcode,
        data: [x11rb::CURRENT_TIME, 0, window, 0, 0].into(),
    };
    conn.send_event(false, tray, EventMask::NO_EVENT, message)
        .map_err(|error| error.to_string())?;
    conn.flush().map_err(|error| error.to_string())?;
    let _ = (ConfigWindow::X, ConfigureWindowAux::new());
    let _ = ChangeWindowAttributesAux::new();
    Ok(IconWindow {
        window,
        tray,
        gc,
        width: ICON,
        height: ICON,
        depth: tray_geom.depth,
        red,
        green,
        blue,
    })
}

fn paint(conn: &RustConnection, icon: &IconWindow) -> Result<(), String> {
    let pixels = bell(icon.width.max(1), icon.height.max(1), icon.red, icon.green, icon.blue);
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        icon.window,
        icon.gc,
        icon.width,
        icon.height,
        0,
        0,
        0,
        icon.depth,
        &pixels,
    )
    .map_err(|error| error.to_string())?;
    conn.flush().map_err(|error| error.to_string())?;
    Ok(())
}

fn bell(width: u16, height: u16, red: u32, green: u32, blue: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let nx = (f32::from(x) + 0.5) / f32::from(width) * 2.0 - 1.0;
            let ny = (f32::from(y) + 0.5) / f32::from(height) * 2.0 - 1.0;
            let bg = (11u8, 110u8, 107u8);
            let mut color = bg;
            let by = ny + 0.02;
            let mut mark = false;
            if (-0.46..=0.20).contains(&by) {
                let t = (by + 0.46) / 0.66;
                if nx.abs() <= 0.08 + t * 0.28 {
                    mark = true;
                }
            }
            if (-0.34..=0.34).contains(&nx) && (0.16..=0.30).contains(&by) {
                mark = true;
            }
            if nx * nx + (by - 0.42) * (by - 0.42) <= 0.07 * 0.07 {
                mark = true;
            }
            if nx * nx + (by + 0.50) * (by + 0.50) <= 0.05 * 0.05 {
                mark = true;
            }
            if mark {
                color = (255, 255, 255);
            }
            let _ = nx * nx + ny * ny;
            data.extend_from_slice(&pack(color.0, color.1, color.2, red, green, blue).to_ne_bytes());
        }
    }
    data
}

fn pack(r: u8, g: u8, b: u8, red: u32, green: u32, blue: u32) -> u32 {
    channel(r, red) | channel(g, green) | channel(b, blue)
}

fn channel(value: u8, mask: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    (u32::from(value) * (mask >> mask.trailing_zeros()).min(255) / 255) << mask.trailing_zeros()
}

fn visual_masks(conn: &RustConnection, screen_num: usize, visual: u32) -> (u32, u32, u32) {
    let screen = &conn.setup().roots[screen_num];
    for depth in &screen.allowed_depths {
        for item in &depth.visuals {
            if item.visual_id == visual {
                return (item.red_mask, item.green_mask, item.blue_mask);
            }
        }
    }
    (0x00ff_0000, 0x0000_ff00, 0x0000_00ff)
}

fn intern(conn: &RustConnection, name: &str) -> Result<u32, String> {
    conn.intern_atom(false, name.as_bytes())
        .map_err(|error| error.to_string())?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| error.to_string())
}
