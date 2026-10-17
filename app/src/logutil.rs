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
                let _ = fs::rename(&self.path, rotated);
            }
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = file.write_all(line.as_bytes());
        }
    }

}
