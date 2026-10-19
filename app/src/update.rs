//! 检查与下载更新。清单和压缩包都从可配置的地址取：
//! http/https 交给系统自带的 curl；其它值按本地路径读，方便本地验证。

use std::path::{Path, PathBuf};
use std::process::Command;

use reminder_core::update::{parse_manifest, verify_sha256_bytes, Manifest};

/// 下载好的包放在数据目录下的 updates/。
pub fn download_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("updates")
}

/// 取清单内容：http(s) 走 curl，其余按本地文件读。
pub fn read_source(url: &str) -> Result<Vec<u8>, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("更新地址为空".into());
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let output = Command::new("curl")
            .args(["-fsSL", "--max-time", "20", url])
            .output()
            .map_err(|error| format!("无法调用 curl：{error}"))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(format!(
                "curl 退出码 {:?}：{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    } else {
        std::fs::read(url).map_err(|error| format!("读取 {url} 失败：{error}"))
    }
}

/// 拉取并解析更新清单。
pub fn fetch_manifest(url: &str) -> Result<Manifest, String> {
    parse_manifest(&read_source(url)?)
}

/// 下载清单指向的压缩包到数据目录，并校验 sha256。返回下载文件的路径。
pub fn download(manifest: &Manifest, data_dir: &Path) -> Result<PathBuf, String> {
    let name = file_name_of(&manifest.url);
    if name.is_empty() {
        return Err("更新清单里的 url 没有文件名".into());
    }
    let dir = download_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|error| format!("无法创建下载目录：{error}"))?;
    let dest = dir.join(&name);

    // 先校验再落盘：sha256 不对就不写文件，避免留下半个坏包。
    let bytes = read_source(&manifest.url)?;
    verify_sha256_bytes(&bytes, &manifest.sha256)?;
    std::fs::write(&dest, &bytes).map_err(|error| format!("写入 {dest:?} 失败：{error}"))?;
    Ok(dest)
}

/// 从 url 里取文件名，去掉 query 和 fragment。
fn file_name_of(url: &str) -> String {
    let trimmed = url.split(['?', '#']).next().unwrap_or(url);
    trimmed
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 用系统文件管理器打开某个目录。
pub fn open_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let (program, args): (&str, Vec<&str>) = ("open", vec![path_str(path)]);
    #[cfg(target_os = "windows")]
    let (program, args): (&str, Vec<&str>) = ("explorer", vec![path_str(path)]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, args): (&str, Vec<&str>) = ("xdg-open", vec![path_str(path)]);

    let output = Command::new(program)
        .args(&args)
        .output()
        .map_err(|error| format!("无法调用 {program}：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{program} 退出码 {:?}", output.status.code()))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_strips_query_and_fragment() {
        assert_eq!(file_name_of("https://e/a/b/reminder.tar.gz"), "reminder.tar.gz");
        assert_eq!(
            file_name_of("https://e/x.zip?token=abc#frag"),
            "x.zip"
        );
        assert_eq!(file_name_of("no-slash"), "no-slash");
    }

    #[test]
    fn download_verifies_sha256_and_writes_into_updates_dir() {
        let root = std::env::temp_dir().join(format!("rd-update-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // 造一个本地“可下载”文件，用路径当 url。
        let payload = root.join("payload.tar.gz");
        std::fs::write(&payload, b"abc").unwrap();
        let manifest = Manifest {
            version: "9.9.9".into(),
            notes: String::new(),
            url: payload.to_string_lossy().into_owned(),
            sha256: reminder_core::update::sha256_hex(b"abc"),
        };
        let saved = download(&manifest, &root).unwrap();
        assert_eq!(saved, download_dir(&root).join("payload.tar.gz"));
        assert!(saved.exists());

        // sha256 不对时必须拒绝，且不落盘（不留下半个坏包）。
        let other = root.join("other.tar.gz");
        std::fs::write(&other, b"abc").unwrap();
        let bad = Manifest {
            sha256: "deadbeef".into(),
            url: other.to_string_lossy().into_owned(),
            ..manifest
        };
        let bad_dest = download_dir(&root).join("other.tar.gz");
        assert!(download(&bad, &root).is_err());
        assert!(!bad_dest.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
