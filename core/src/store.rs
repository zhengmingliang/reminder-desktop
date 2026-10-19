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
    repaired: bool,
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
                    repaired,
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
                repaired: false,
            },
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 主文件损坏、这次是用上一份备份恢复出来的。
    pub fn repaired_from_backup(&self) -> bool {
        self.repaired
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

    /// 每天最多留一份自动快照（文件名只带日期）；当天已经有了就跳过。
    /// 万一某次改动写坏了，除了 `.bak` 的一步回退，还能按天往回找。
    pub fn daily_backup(&self) -> Result<Option<PathBuf>, String> {
        let dir = self.backup_dir();
        let path = dir.join(format!(
            "reminders-{}.json",
            chrono::Local::now().format("%Y%m%d")
        ));
        if path.exists() {
            return Ok(None);
        }
        let bytes = encode(&self.reminders)?;
        atomic_write(&path, &bytes)?;
        prune_backups(&dir, KEEP_BACKUPS);
        Ok(Some(path))
    }

    /// 把当前内存里的提醒写成一份带时间戳的副本，不替换正在使用的文件。
    /// 只保留最近 `KEEP_BACKUPS` 份，更早的副本会被删除。
    pub fn backup(&self) -> Result<PathBuf, String> {
        let dir = self.backup_dir();
        let path = fresh_backup_path(&dir);
        let bytes = encode(&self.reminders)?;
        atomic_write(&path, &bytes)?;
        prune_backups(&dir, KEEP_BACKUPS);
        Ok(path)
    }

    /// 已有的备份文件，最新的排在前面。
    pub fn list_backups(&self) -> Vec<PathBuf> {
        list_backup_files(&self.backup_dir())
    }

    /// 用一份备份替换当前提醒。替换前会先备份当前数据，恢复后仍可撤回。
    /// 返回恢复后的提醒条数。
    pub fn restore(&mut self, backup: &Path) -> Result<usize, String> {
        self.ensure_writable()?;
        let data = read_data(backup)?;
        if data.version == 0 || data.version > VERSION {
            return Err(format!("这份备份的版本是 {}，当前程序无法读取", data.version));
        }
        self.backup()?;
        self.reminders = data.reminders;
        self.write_all()?;
        Ok(self.reminders.len())
    }

    fn backup_dir(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
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

const KEEP_BACKUPS: usize = 10;

/// 备份文件名按毫秒递增，同一毫秒内的重复备份会顺延，避免覆盖已有副本。
fn fresh_backup_path(dir: &Path) -> PathBuf {
    let mut at = chrono::Local::now();
    loop {
        let name = format!("reminders-{}.json", at.format("%Y%m%d-%H%M%S%3f"));
        let path = dir.join(name);
        if !path.exists() {
            return path;
        }
        at += chrono::Duration::milliseconds(1);
    }
}

fn is_backup_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("reminders-") && name.ends_with(".json"))
}

/// 文件名里带时间戳，字典序就是时间顺序；返回结果最新的在前。
fn list_backup_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| is_backup_name(path))
        .collect();
    files.sort();
    files.reverse();
    files
}

fn prune_backups(dir: &Path, keep: usize) {
    for old in list_backup_files(dir).into_iter().skip(keep) {
        let _ = fs::remove_file(old);
    }
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
        assert!(
            recovered.repaired_from_backup(),
            "从备份恢复出来时要能被调用方发现"
        );
        assert!(read_data(&path).is_ok(), "损坏的主文件应从备份恢复");
        assert!(!Store::open(path.clone()).repaired_from_backup(), "文件正常时不算修复");

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
            Schedule::Cron { expression, .. } => assert_eq!(expression, "0 18 * * 1-5"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn backups_are_pruned_to_the_newest_ones() {
        let path = temp_path("prune");
        let dir = path.parent().unwrap().to_path_buf();
        for i in 0..KEEP_BACKUPS + 3 {
            fs::write(dir.join(format!("reminders-20260101-{i:06}.json")), b"{}").unwrap();
        }
        prune_backups(&dir, KEEP_BACKUPS);
        let left = list_backup_files(&dir);
        assert_eq!(left.len(), KEEP_BACKUPS);
        assert!(left[0].ends_with("reminders-20260101-000012.json"));
        assert!(left[KEEP_BACKUPS - 1].ends_with("reminders-20260101-000003.json"));
    }

    #[test]
    fn daily_backup_is_written_once_per_day_and_can_be_listed() {
        let path = temp_path("daily");
        let mut store = Store::open(path.clone());
        store.upsert(one()).unwrap();
        let first = store.daily_backup().unwrap().expect("首日应生成快照");
        assert_eq!(
            first.file_name().unwrap().to_string_lossy(),
            format!("reminders-{}.json", Local::now().format("%Y%m%d"))
        );
        assert_eq!(read_data(&first).unwrap().reminders.len(), 1);
        assert!(store.daily_backup().unwrap().is_none(), "同一天只留一份");
        assert!(store.list_backups().contains(&first));
    }

    #[test]
    fn restore_replaces_reminders_and_keeps_a_pre_restore_backup() {
        let path = temp_path("restore");
        let mut store = Store::open(path.clone());
        let reminder = one();
        let id = reminder.id.clone();
        store.upsert(reminder).unwrap();
        let saved = store.backup().unwrap();
        store.delete(&id).unwrap();
        assert!(store.reminders.is_empty());
        let count = store.restore(&saved).unwrap();
        assert_eq!(count, 1);
        assert_eq!(Store::open(path).reminders.len(), 1);
        assert!(saved.exists(), "恢复不能删除被恢复的备份");
        let backups = store.list_backups();
        assert_eq!(backups.len(), 2);
        assert_ne!(backups[0], saved);
        assert!(read_data(&backups[0]).unwrap().reminders.is_empty());
    }
}
