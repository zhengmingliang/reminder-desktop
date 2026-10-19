//! Wayland 会话下的兜底提醒：在弹窗之外再发一条系统通知。
//! 纯 Wayland 拿不到全局光标坐标，弹窗的定位与置顶也不一定被合成器尊重，
//! 因此到期提醒在 Wayland 下额外发一条系统通知，确保用户一定看得到。
//! X11 / Windows / macOS 上弹窗行为可靠，默认不发；可用环境变量强制开启便于排查。

use std::process::Command;

/// 是否需要发送系统通知兜底。
pub fn enabled() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return true;
    }
    // 排查用：任意会话下强制开启。
    std::env::var("REMINDER_SYSTEM_NOTIFY")
        .is_ok_and(|value| value == "1" || value == "always")
}

/// 发送一条系统通知。失败返回错误说明，由调用方记日志，不影响弹窗本身。
pub fn send(title: &str, body: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        send_linux(title, body)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (title, body);
        Err("当前平台没有系统通知兜底通道".into())
    }
}

#[cfg(target_os = "linux")]
fn send_linux(title: &str, body: &str) -> Result<(), String> {
    // 危急级别让通知停留在屏幕上，直到用户处理；固定应用名，便于通知中心归类。
    let output = Command::new("notify-send")
        .args(["-a", "桌面提醒", "-u", "critical", title, body])
        .output()
        .map_err(|error| format!("无法调用 notify-send：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("notify-send 退出码 {:?}：{stderr}", output.status.code()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_env_forces_notifications_on() {
        // 该环境变量仅此处使用，测前清空、测后还原，避免影响其它测试。
        let saved = std::env::var("REMINDER_SYSTEM_NOTIFY").ok();
        std::env::set_var("REMINDER_SYSTEM_NOTIFY", "1");
        assert!(enabled());
        // Wayland 会话本身就会开启兜底，关闭断言只在非 Wayland 下成立。
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            std::env::set_var("REMINDER_SYSTEM_NOTIFY", "off");
            assert!(!enabled());
        }
        match saved {
            Some(value) => std::env::set_var("REMINDER_SYSTEM_NOTIFY", value),
            None => std::env::remove_var("REMINDER_SYSTEM_NOTIFY"),
        }
    }
}
