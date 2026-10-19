//! 回查会话总线，确认 StatusNotifierItem 真的挂上去了。
//!
//! Slint 的 `SystemTrayIcon` 在 Linux 上走 ksni。ksni 在会话里没有
//! `org.kde.StatusNotifierWatcher` 宿主时注册失败，而这个失败被 Slint 内部吞掉
//! （只经 `debug_log!` 打到 stderr），`AppTray::new()` 照样返回 `Ok`。也就是说
//! 「创建成功」并不等于图标真的出现在托盘上，只能回查总线才能分清。
//!
//! ksni 会为自己申请 `org.kde.StatusNotifierItem-<pid>-<序号>` 这个总线名；注册
//! 失败时整条连接随即释放，名字也就跟着消失。所以名字还在，就说明宿主收下了图标。

use std::sync::OnceLock;

use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::Connection;

/// 会话总线连接只建一次：每轮重试都重连会白白多出一条 D-Bus 连接和它的后台线程。
static CONNECTION: OnceLock<Option<Connection>> = OnceLock::new();

fn connection() -> Option<&'static Connection> {
    CONNECTION.get_or_init(|| Connection::session().ok()).as_ref()
}

/// 本进程有没有一个活着的 StatusNotifierItem。
pub fn registered() -> bool {
    let Some(connection) = connection() else {
        return false;
    };
    let Ok(proxy) = DBusProxy::new(connection) else {
        return false;
    };
    let Ok(names) = proxy.list_names() else {
        return false;
    };
    let prefix = format!("org.kde.StatusNotifierItem-{}-", std::process::id());
    names.iter().any(|name| name.as_str().starts_with(&prefix))
}

#[cfg(test)]
mod tests {
    use super::{connection, registered};
    use zbus::blocking::fdo::DBusProxy;
    use zbus::fdo::RequestNameFlags;
    use zbus::names::WellKnownName;

    /// 真去总线上申请一个 ksni 会用的那种名字，验证「名字出现 = 注册成功」这条判断成立。
    /// 没有会话总线的环境（纯终端、CI）申请不到名字，跳过而不算失败。
    #[test]
    fn detects_the_name_ksni_would_claim() {
        let Some(connection) = connection() else {
            eprintln!("没有会话总线，跳过");
            return;
        };
        let Ok(proxy) = DBusProxy::new(connection) else {
            return;
        };
        assert!(!registered(), "还没有 SNI 名字时不该判为已注册");
        let owned = format!("org.kde.StatusNotifierItem-{}-test", std::process::id());
        let Ok(name) = WellKnownName::try_from(owned.as_str()) else {
            return;
        };
        if proxy
            .request_name(name.clone(), RequestNameFlags::DoNotQueue.into())
            .is_err()
        {
            eprintln!("申请总线名失败，跳过");
            return;
        }
        assert!(registered(), "本进程的 SNI 名字出现后应判为已注册");
        let _ = proxy.release_name(name);
        assert!(!registered(), "名字释放后不该再判为已注册");
    }
}
