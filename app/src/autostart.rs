//! 开机自启动：Linux 写 XDG autostart 文件，macOS 写 LaunchAgent，
//! Windows 写当前用户的 Run 注册表项。只影响当前用户，不需要管理员权限。

use std::path::PathBuf;
#[cfg(not(target_os = "windows"))]
use std::{fs, io};

const APP_ID: &str = "reminder-desktop";
#[cfg(target_os = "macos")]
const LABEL: &str = "com.reminder-desktop";
#[cfg(target_os = "windows")]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub fn is_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("reg")
            .args(["query", RUN_KEY, "/v", APP_ID])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        entry_path().map(|path| path.is_file()).unwrap_or(false)
    }
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| format!("无法读取程序路径：{error}"))?;
    let exe = exe.to_string_lossy().into_owned();
    #[cfg(target_os = "windows")]
    {
        let status = if on {
            std::process::Command::new("reg")
                .args(["add", RUN_KEY, "/v", APP_ID, "/t", "REG_SZ", "/d"])
                .arg(format!("\"{exe}\""))
                .arg("/f")
                .status()
        } else {
            std::process::Command::new("reg")
                .args(["delete", RUN_KEY, "/v", APP_ID, "/f"])
                .status()
        };
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("注册表写入失败（{status}）")),
            Err(error) => Err(format!("无法运行 reg：{error}")),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let path = entry_path()?;
        if on {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| format!("无法创建自启动目录：{error}"))?;
            }
            fs::write(&path, entry_text(&exe)).map_err(|error| format!("无法写入自启动文件：{error}"))
        } else {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!("无法删除自启动文件：{error}")),
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "找不到用户目录".to_string())
}

#[cfg(target_os = "linux")]
fn entry_path() -> Result<PathBuf, String> {
    let config = match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => home()?.join(".config"),
    };
    Ok(config.join("autostart").join(format!("{APP_ID}.desktop")))
}

#[cfg(target_os = "macos")]
fn entry_path() -> Result<PathBuf, String> {
    Ok(home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "linux")]
fn entry_text(exe: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=桌面提醒\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_exec(exe)
    )
}

#[cfg(target_os = "macos")]
fn entry_text(exe: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
\t<key>Label</key>\n\t<string>{LABEL}</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{}</string>\n\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\
</dict>\n</plist>\n",
        xml_escape(exe)
    )
}

/// Exec 字段：路径用双引号包起来，并转义引号内有特殊含义的字符；`%` 要写成 `%%`。
#[cfg(any(target_os = "linux", test))]
fn desktop_exec(exe: &str) -> String {
    let escaped = exe
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{desktop_exec, xml_escape};

    #[test]
    fn desktop_exec_quotes_and_escapes_path() {
        assert_eq!(desktop_exec("/opt/My App/reminder"), "\"/opt/My App/reminder\"");
        assert_eq!(desktop_exec("/a\"b$c%d"), "\"/a\\\"b\\$c%%d\"");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enable_then_disable_writes_and_removes_desktop_file() {
        let home = std::env::temp_dir().join(format!("rd-autostart-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        std::env::remove_var("XDG_CONFIG_HOME");

        super::set_enabled(true).unwrap();
        assert!(super::is_enabled());
        let text = std::fs::read_to_string(home.join(".config/autostart/reminder-desktop.desktop")).unwrap();
        assert!(text.contains("Exec=\""));
        super::set_enabled(false).unwrap();
        assert!(!super::is_enabled());
        super::set_enabled(false).unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn xml_escape_handles_reserved_characters() {
        assert_eq!(xml_escape("a&b<c>\"d\""), "a&amp;b&lt;c&gt;&quot;d&quot;");
    }
}
