use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Logger {
    path: PathBuf,
    file: Mutex<()>,
}

impl Logger {
    pub fn open(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self { path, file: Mutex::new(()) }
    }

    pub fn log(&self, level: &str, message: &str) {
        let line = format!(
            "{} {level} {message}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        let Ok(_guard) = self.file.lock() else {
            return;
        };
        if let Ok(meta) = fs::metadata(&self.path) {
            if meta.len() > 1_048_576 {
                let rotated = self.path.with_extension("log.1");
                // Windows 上 fs::rename 不会覆盖已存在的目标文件，旧的一份要先删掉，
                // 否则第二次轮转失败、app.log 会一直变大。
                let _ = fs::remove_file(&rotated);
                let _ = fs::rename(&self.path, rotated);
            }
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Logger;
    use std::fs;

    #[test]
    fn rotates_oversized_log_repeatedly_and_stays_bounded() {
        let dir = std::env::temp_dir().join(format!("rd-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("app.log");
        let rotated = path.with_extension("log.1");
        let logger = Logger::open(path.clone());

        // 写满一次后再次写入，应触发轮转，留下 app.log.1，app.log 重新开始。
        logger.log("INFO", &"x".repeat(1_100_000));
        logger.log("INFO", "after-first");
        assert!(rotated.is_file(), "超过 1 MB 后应生成 app.log.1");
        assert!(fs::metadata(&path).unwrap().len() < 1_048_576);

        // 再写满一次：验证可重复轮转（Windows 覆盖旧文件的问题）。
        logger.log("INFO", &"y".repeat(1_100_000));
        logger.log("INFO", "after-second");
        assert!(rotated.is_file(), "第二次轮转仍应保留 app.log.1");
        assert!(fs::metadata(&path).unwrap().len() < 1_048_576);

        let _ = fs::remove_dir_all(&dir);
    }
}
