use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;

use rodio::Decoder;

pub const DEFAULT_CHIME: &[u8] = include_bytes!("../assets/default-chime.wav");
const MAX_SOUND_BYTES: u64 = 15 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum AudioSource {
    Default,
    File(std::path::PathBuf),
}

impl AudioSource {
    pub fn from_path(path: &str) -> Self {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            Self::Default
        } else {
            Self::File(std::path::PathBuf::from(trimmed))
        }
    }
}

pub fn check_sound_file(path: &str) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let file_path = Path::new(trimmed);
    let meta = std::fs::metadata(file_path).map_err(|_| {
        format!("铃声文件不存在：{trimmed}")
    })?;
    if !meta.is_file() {
        return Err("铃声路径不是文件".into());
    }
    if meta.len() > MAX_SOUND_BYTES {
        return Err("铃声文件不能超过 15 MB".into());
    }
    if meta.len() == 0 {
        return Err("铃声文件是空的".into());
    }
    let ext = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "wav" | "mp3" | "ogg" | "flac") {
        return Err("铃声仅支持 wav、mp3、ogg、flac".into());
    }
    let file = File::open(file_path).map_err(|error| format!("无法读取铃声：{error}"))?;
    ensure_decodable(BufReader::new(file))?;
    Ok(())
}

pub fn read_sound(source: &AudioSource) -> Result<Vec<u8>, String> {
    match source {
        AudioSource::Default => Ok(DEFAULT_CHIME.to_vec()),
        AudioSource::File(path) => {
            check_sound_file(&path.to_string_lossy())?;
            let mut file = File::open(path).map_err(|error| format!("无法读取铃声：{error}"))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|error| format!("无法读取铃声：{error}"))?;
            Ok(bytes)
        }
    }
}

pub fn decode_bytes(bytes: &[u8]) -> Result<(), String> {
    ensure_decodable(Cursor::new(bytes.to_vec()))
}

fn ensure_decodable<R>(reader: R) -> Result<(), String>
where
    R: Read + std::io::Seek + Send + Sync + 'static,
{
    let mut decoder = Decoder::new(reader).map_err(|error| error.to_string())?;
    if decoder.next().is_none() {
        return Err("音频里没有可播放的采样".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chime_decodes() {
        decode_bytes(DEFAULT_CHIME).unwrap();
    }

    #[test]
    fn empty_path_means_default() {
        assert!(check_sound_file("  ").is_ok());
        assert!(check_sound_file("/no/such/file.wav").unwrap_err().contains("不存在"));
        assert!(check_sound_file("/dev/null").is_err());
    }
}
