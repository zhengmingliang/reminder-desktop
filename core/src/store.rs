use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Reminder;

const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileData {
    version: u32,
    reminders: Vec<Reminder>,
}

pub struct Store {
    path: PathBuf,
    pub reminders: Vec<Reminder>,
    persist_blocked: Option<String>,
}

impl Store {
    pub fn open(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match load_file(&path) {
            Load::Ready { reminders, repaired } => {
                let store = Self {
                    path,
                    reminders,
                    persist_blocked: None,
                };
                if repaired {
                    let _ = store.write_all();
                }
                store
            }
            Load::Blocked(reason) => Self {
                path,
                reminders: Vec::new(),
                persist_blocked: Some(reason),
            },
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn persist_block_reason(&self) -> Option<&str> {
        self.persist_blocked.as_deref()
    }

    pub fn upsert(&mut self, reminder: Reminder) -> Result<(), String> {
        self.ensure_writable()?;
        if let Some(index) = self.reminders.iter().position(|item| item.id == reminder.id) {
            self.reminders[index] = reminder;
        } else {
            self.reminders.push(reminder);
        }
        self.write_all()
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.ensure_writable()?;
        let before = self.reminders.len();
        self.reminders.retain(|item| item.id != id);
        if self.reminders.len() == before {
            return Err("这条提醒已经不存在".into());
        }
        self.write_all()
    }

    pub fn save(&mut self) -> Result<(), String> {
        self.ensure_writable()?;
        self.write_all()
    }

    /// 把当前内存里的提醒写成一份带时间戳的副本，不替换正在使用的文件。
    pub fn backup(&self) -> Result<PathBuf, String> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let name = format!("reminders-{stamp}.json");
        let path = self
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(name);
        let bytes = encode(&self.reminders)?;
        atomic_write(&path, &bytes)?;
        Ok(path)
    }

    fn ensure_writable(&self) -> Result<(), String> {
        if let Some(reason) = &self.persist_blocked {
            return Err(reason.clone());
        }
        Ok(())
    }

    fn write_all(&self) -> Result<(), String> {
        let bytes = encode(&self.reminders)?;
        atomic_write(&self.path, &bytes)
    }
}

fn encode(reminders: &[Reminder]) -> Result<Vec<u8>, String> {
    let data = FileData {
        version: VERSION,
        reminders: reminders.to_vec(),
    };
    serde_json::to_vec_pretty(&data).map_err(|error| format!("无法序列化提醒数据：{error}"))
}

enum Load {
    Ready { reminders: Vec<Reminder>, repaired: bool },
    Blocked(String),
}

fn load_file(path: &Path) -> Load {
    if !path.exists() {
        return Load::Ready {
            reminders: Vec::new(),
            repaired: false,
        };
    }
    match read_data(path) {
        Ok(data) => accept(path, data, false),
        Err(error) => {
            let bak = backup_path(path);
            if bak.exists() {
                if let Ok(data) = read_data(&bak) {
                    return accept(path, data, true);
                }
            }
            quarantine(path);
            Load::Blocked(format!(
                "提醒数据损坏，已保留原文件并停止写入，避免覆盖。{error}"
            ))
        }
    }
}

fn accept(path: &Path, data: FileData, repaired: bool) -> Load {
    if data.version == 0 || data.version > VERSION {
        Load::Blocked(format!(
            "数据文件版本是 {}，当前程序只认识版本 {VERSION}。为避免覆盖，已暂停保存。文件：{}",
            data.version,
            path.display()
        ))
    } else {
        Load::Ready {
            reminders: data.reminders,
            repaired,
        }
    }
}

fn read_data(path: &Path) -> Result<FileData, String> {
    let mut file = File::open(path).map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("JSON 无法解析：{error}"))
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "reminders.json".into());
    name.push(".bak");
    path.with_file_name(name)
}

fn quarantine(path: &Path) {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "reminders.json".into());
    name.push(".broken");
    let target = path.with_file_name(name);
    let _ = fs::rename(path, target);
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建目录：{error}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|error| format!("无法写入临时文件：{error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("无法写入临时文件：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("无法刷盘：{error}"))?;
    }
    // 只把还能解析的旧文件留作备份，避免修复损坏文件时把坏内容写进 .bak。
    if path.exists() && read_data(path).is_ok() {
        let bak = backup_path(path);
        let _ = fs::copy(path, bak);
    }
    fs::rename(&tmp, path).map_err(|error| format!("无法替换数据文件：{error}"))?;
    Ok(())
}

pub fn default_data_dir() -> PathBuf {
    if let Some(dirs) = directories_fallback() {
        return dirs;
    }
    PathBuf::from("reminder-desktop-data")
}

fn directories_fallback() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("APPDATA")?;
        return Some(PathBuf::from(base).join("ReminderDesktop"));
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        return Some(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("ReminderDesktop"),
        );
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(xdg).join("reminder-desktop"));
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".local").join("share").join("reminder-desktop"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Draft, Schedule};
    use crate::validate::build_reminder;
    use chrono::Local;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("reminder-desktop-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir.join("reminders.json")
    }

    fn one() -> Reminder {
        let mut draft = Draft::blank();
        draft.title = "测试".into();
        draft.minutes = "5".into();
        build_reminder(&draft, None, Local::now(), 0).unwrap()
    }

    #[test]
    fn roundtrip_and_backup_survive_reload() {
        let path = temp_path("round");
        let mut store = Store::open(path.clone());
        let reminder = one();
        let id = reminder.id.clone();
        store.upsert(reminder.clone()).unwrap();
        store.upsert(reminder).unwrap();
        assert!(backup_path(&path).exists(), "第二次保存应留下上一份备份");
        let loaded = Store::open(path.clone());
        assert_eq!(loaded.reminders.len(), 1);
        assert_eq!(loaded.reminders[0].id, id);
        let copy = loaded.backup().unwrap();
        assert!(copy.exists());
        store.delete(&id).unwrap();
        assert!(Store::open(path).reminders.is_empty());
    }

    #[test]
    fn corrupt_main_file_falls_back_to_bak_and_blocks_when_both_bad() {
        let path = temp_path("corrupt");
        let mut store = Store::open(path.clone());
        store.upsert(one()).unwrap();
        store.upsert(one()).unwrap();
        fs::write(&path, b"{ this is not json").unwrap();
        let recovered = Store::open(path.clone());
        assert_eq!(recovered.reminders.len(), 1);
        assert!(recovered.persist_block_reason().is_none());
        assert!(read_data(&path).is_ok(), "损坏的主文件应从备份恢复");

        fs::write(&path, b"not-json").unwrap();
        fs::write(backup_path(&path), b"also-not").unwrap();
        let blocked = Store::open(path.clone());
        assert!(blocked.reminders.is_empty());
        assert!(blocked.persist_block_reason().unwrap().contains("损坏"));
        let error = {
            let mut blocked = blocked;
            blocked.upsert(one())
        };
        assert!(error.is_err());
        assert!(path.with_file_name("reminders.json.broken").exists() || !path.exists());
    }

    #[test]
    fn newer_version_is_not_overwritten() {
        let path = temp_path("version");
        fs::write(&path, br#"{"version":9,"reminders":[]}"#).unwrap();
        let mut store = Store::open(path.clone());
        assert!(store.persist_block_reason().unwrap().contains("版本"));
        assert!(store.upsert(one()).is_err());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 9") || text.contains("\"version\":9"));
    }

    #[test]
    fn schedule_field_roundtrips() {
        let path = temp_path("cron");
        let mut draft = Draft::blank();
        draft.title = "日报".into();
        draft.kind = crate::model::Kind::Cron;
        draft.cron = "0 18 * * 1-5".into();
        let reminder = build_reminder(&draft, None, Local::now(), 0).unwrap();
        let mut store = Store::open(path.clone());
        store.upsert(reminder).unwrap();
        let loaded = Store::open(path);
        match &loaded.reminders[0].schedule {
            Schedule::Cron { expression } => assert_eq!(expression, "0 18 * * 1-5"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
