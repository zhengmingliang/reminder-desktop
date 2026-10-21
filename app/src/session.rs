//! 桌面会话类型探测。
//!
//! Linux 下托盘只有两条路可走：XEmbed（要一个真的 X 服务器，由面板持有
//! `_NET_SYSTEM_TRAY_S0` 选择权）和 StatusNotifierItem（要会话总线上有
//! `org.kde.StatusNotifierWatcher` 宿主）。纯 Wayland 会话里第一条根本不可能成立，
//! 所以先分清会话类型，再决定要不要白跑一趟 XEmbed 的探测，以及托盘没出来时
//! 该怎么向用户解释。
//!
//! Windows 和 macOS 没有这套问题：托盘由系统原生接口注册（Windows 是
//! `Shell_NotifyIconW`，macOS 是菜单栏状态项），这里的探测只为设置面板的文案。

/// 当前图形会话的类型。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionKind {
    /// X11，或者带 XWayland 的会话（X 那套协议仍可尝试）。
    X11,
    /// 纯 Wayland：有 `WAYLAND_DISPLAY` 但没有 `DISPLAY`。
    Wayland,
    /// 两个变量都没有：多半是无图形界面的终端 / SSH 会话。
    Unknown,
    /// Windows：托盘走 Shell_NotifyIconW 原生通知区。
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Windows,
    /// macOS：托盘是菜单栏右侧的状态项。
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    MacOS,
}

impl SessionKind {
    /// 设置面板里给用户的会话名。
    pub fn label(self) -> &'static str {
        match self {
            SessionKind::X11 => "X11",
            SessionKind::Wayland => "Wayland",
            SessionKind::Unknown => "未知",
            SessionKind::Windows => "Windows",
            SessionKind::MacOS => "macOS",
        }
    }

    /// XEmbed 需要 X 服务器；纯 Wayland、Windows、macOS 上一律不可能，连探测都不必做。
    pub fn xembed_possible(self) -> bool {
        matches!(self, SessionKind::X11 | SessionKind::Unknown)
    }
}

/// 环境变量为空等同于没设置：`DISPLAY=` 这种空值在登录脚本里并不少见。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn env_set(name: &str) -> bool {
    std::env::var_os(name).map(|value| !value.is_empty()).unwrap_or(false)
}

#[cfg(target_os = "windows")]
pub fn kind() -> SessionKind {
    SessionKind::Windows
}

#[cfg(target_os = "macos")]
pub fn kind() -> SessionKind {
    SessionKind::MacOS
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn kind() -> SessionKind {
    let xdg = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    classify(&xdg, env_set("WAYLAND_DISPLAY"), env_set("DISPLAY"))
}

/// 把探测规则单独抽出来，方便不碰环境变量就能测：`XDG_SESSION_TYPE` 优先，
/// 没有或是不认识的值时才看 `WAYLAND_DISPLAY` / `DISPLAY` 的组合。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn classify(xdg: &str, wayland: bool, x11: bool) -> SessionKind {
    match xdg.trim().to_ascii_lowercase().as_str() {
        "wayland" => return SessionKind::Wayland,
        "x11" => return SessionKind::X11,
        _ => {}
    }
    match (wayland, x11) {
        // 同时有 DISPLAY 说明 XWayland 在跑，X 那套协议仍值得一试。
        (true, false) => SessionKind::Wayland,
        (true, true) => SessionKind::X11,
        (false, true) => SessionKind::X11,
        (false, false) => SessionKind::Unknown,
    }
}

#[cfg(all(test, not(any(target_os = "windows", target_os = "macos"))))]
mod tests {
    use super::{classify, SessionKind};

    #[test]
    fn xdg_session_type_wins() {
        // KDE / GNOME 的 Wayland 会话里 XWayland 也在，DISPLAY 是有值的，
        // 只有 XDG_SESSION_TYPE 能说清楚这是 Wayland。
        assert_eq!(classify("wayland", true, true), SessionKind::Wayland);
        assert_eq!(classify("x11", true, true), SessionKind::X11);
        assert_eq!(classify("X11", false, false), SessionKind::X11);
    }

    #[test]
    fn pure_wayland_has_no_display() {
        assert_eq!(classify("", true, false), SessionKind::Wayland);
        assert!(!SessionKind::Wayland.xembed_possible());
    }

    #[test]
    fn xwayland_still_tries_xembed() {
        assert_eq!(classify("", true, true), SessionKind::X11);
        assert!(SessionKind::X11.xembed_possible());
        // 探测不到会话时也别把 XEmbed 一棍子打死，交给 x11rb 自己连一次。
        assert_eq!(classify("tty", false, false), SessionKind::Unknown);
        assert!(SessionKind::Unknown.xembed_possible());
    }
}
