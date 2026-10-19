//! 用户偏好的持久化：目前只有主题模式，落盘到数据目录下的 settings.json。
//! 与提醒数据分开存，读写都走「先写临时文件再改名」，避免写一半崩溃留坏文件。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SETTINGS_FILE: &str = "settings.json";

/// 主题模式：浅色 / 跟随系统 / 深色。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    System,
    Dark,
}

impl Default for ThemeMode {
    /// 默认跟随系统。
    fn default() -> Self {
        ThemeMode::System
    }
}

impl ThemeMode {
    /// 设置界面里用 0/1/2 传给 Slint（浅色 / 跟随系统 / 深色）。
    pub fn as_index(self) -> i32 {
        match self {
            ThemeMode::Light => 0,
            ThemeMode::System => 1,
            ThemeMode::Dark => 2,
        }
    }

    pub fn from_index(index: i32) -> ThemeMode {
        match index {
            0 => ThemeMode::Light,
            2 => ThemeMode::Dark,
            _ => ThemeMode::System,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: ThemeMode,
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SETTINGS_FILE)
}

/// 读取设置；文件不存在或内容损坏时回退到默认值（跟随系统）。
pub fn load_settings(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_settings(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(settings).unwrap_or_else(|_| b"{}".to_vec());
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_corrupt_file_falls_back_to_follow_system() {
        let dir = std::env::temp_dir().join(format!("rd-settings-{}", std::process::id()));
        let path = settings_path(&dir);
        assert_eq!(load_settings(&path).theme, ThemeMode::System);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load_settings(&path).theme, ThemeMode::System);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_choice_roundtrips_and_uses_temp_rename() {
        let dir = std::env::temp_dir().join(format!("rd-settings-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = settings_path(&dir);
        save_settings(&path, &Settings { theme: ThemeMode::Dark }).unwrap();
        assert_eq!(load_settings(&path).theme, ThemeMode::Dark);
        // 临时文件不应残留。
        assert!(!path.with_extension("json.tmp").exists());
        assert_eq!(ThemeMode::from_index(2), ThemeMode::Dark);
        assert_eq!(ThemeMode::from_index(0).as_index(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
