use std::fs::{self, File, OpenOptions};
use std::path::Path;

use fs2::FileExt;

pub struct InstanceLock {
    _file: File,
}

pub enum Launch {
    Primary(InstanceLock),
    Secondary,
}

pub fn claim(dir: &Path) -> Result<Launch, String> {
    match try_primary(dir)? {
        Some(lock) => Ok(Launch::Primary(lock)),
        None => {
            let _ = fs::write(dir.join("show.request"), b"1\n");
            Ok(Launch::Secondary)
        }
    }
}

/// 试着独占实例锁：拿到（返回 Some）说明当前没有实例在跑；拿不到（返回 None）说明已有实例。
/// 不会像 `claim` 那样写「打开主窗口」的请求文件，命令行子进程用它来判断该直接写库还是交给运行中的界面。
pub fn try_primary(dir: &Path) -> Result<Option<InstanceLock>, String> {
    fs::create_dir_all(dir).map_err(|error| format!("无法创建数据目录：{error}"))?;
    let path = dir.join("instance.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("无法创建实例锁：{error}"))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(_) => Ok(None),
    }
}
