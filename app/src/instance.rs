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
        Ok(()) => Ok(Launch::Primary(InstanceLock { _file: file })),
        Err(_) => {
            let _ = fs::write(dir.join("show.request"), b"1\n");
            Ok(Launch::Secondary)
        }
    }
}
