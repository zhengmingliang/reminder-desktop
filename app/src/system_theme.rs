//! 探测系统当前是否处于深色外观。
//! 各桌面环境没有统一接口，这里按平台用常见命令兜底；探测不到一律按浅色处理。

use std::process::Command;

/// 系统是否为深色。
pub fn detect_dark() -> bool {
    #[cfg(target_os = "linux")]
    {
        detect_dark_linux()
    }
    #[cfg(target_os = "windows")]
    {
        detect_dark_windows()
    }
    #[cfg(target_os = "macos")]
    {
        detect_dark_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn detect_dark_linux() -> bool {
    // GTK_THEME 形如 Adwaita:dark。
    if let Ok(theme) = std::env::var("GTK_THEME") {
        return theme.to_ascii_lowercase().contains("dark");
    }
    // GNOME / 支持 color-scheme 的桌面。
    if let Some(value) = run("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"]) {
        if value.contains("prefer-dark") {
            return true;
        }
        if value.contains("prefer-light") {
            return false;
        }
    }
    // 回退到主题名里带 dark 的判断（Cinnamon / XFCE 常见）。
    if let Some(value) = run("gsettings", &["get", "org.gnome.desktop.interface", "gtk-theme"]) {
        let lower = value.to_ascii_lowercase();
        if lower.contains("dark") {
            return true;
        }
        if lower.contains("light") {
            return false;
        }
    }
    false
}

#[cfg(target_os = "windows")]
fn detect_dark_windows() -> bool {
    // AppsUseLightTheme=0x0 表示深色、0x1 表示浅色。
    run(
        "reg",
        &[
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "/v",
            "AppsUseLightTheme",
        ],
    )
    .map(|value| value.contains("0x0"))
    .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn detect_dark_macos() -> bool {
    // 浅色时该键不存在，命令返回非 0，视为浅色。
    run("defaults", &["read", "-g", "AppleInterfaceStyle"])
        .map(|value| value.to_ascii_lowercase().contains("dark"))
        .unwrap_or(false)
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
