//! 检查与下载更新。清单和压缩包都从可配置的地址取：
//! http/https 交给系统自带的 curl；其它值按本地路径读，方便本地验证。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// 打开发布包，把里面的可执行文件取出来放到 dest 下，返回它的路径。
/// 发布包是 tar.gz（Linux / macOS）或 zip（Windows），都交给系统自带的解压命令，
/// 省下两个解压依赖的编译时间。
pub fn unpack_binary(archive: &Path, dest: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dest).map_err(|error| format!("无法创建解压目录：{error}"))?;
    let name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".zip") {
        // Windows 自带的 tar（bsdtar）能解 zip；其它平台先试 unzip，没有再退到 tar。
        if run_quiet("unzip", ["-o", path_str(archive), "-d", path_str(dest)]).is_ok() {
            // ok
        } else {
            run_quiet("tar", ["-xf", path_str(archive), "-C", path_str(dest)])?;
        }
    } else {
        run_quiet("tar", ["-xzf", path_str(archive), "-C", path_str(dest)])?;
    }
    find_binary(dest).ok_or_else(|| "压缩包里没找到 reminder-desktop 可执行文件".to_string())
}

fn run_quiet(program: &str, args: [&str; 4]) -> Result<(), String> {
    // 参数里的路径借用自调用方，这里只是拼命令行，生命周期够用。
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("无法调用 {program}：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} 退出码 {:?}：{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// 在解压目录里找可执行文件（包里可能套了一层目录）。
fn find_binary(dir: &Path) -> Option<PathBuf> {
    let wanted = if cfg!(windows) { "reminder-desktop.exe" } else { "reminder-desktop" };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if entry.file_name().to_string_lossy() == wanted {
                return Some(path);
            }
        }
    }
    None
}

/// 用下载来的新版本替换当前程序并重启。
/// 步骤：把正在运行的这个先改名留一份 .old（失败可手动换回来），再把新文件放到原位置，
/// 接着拉起新进程（带 --after-update），最后自己退出。
pub fn replace_and_restart(new_binary: &Path) -> Result<(), String> {
    let current = std::env::current_exe().map_err(|error| format!("找不到当前程序路径：{error}"))?;
    let backup = backup_path(&current);
    let _ = fs::remove_file(&backup);
    fs::rename(&current, &backup)
        .map_err(|error| format!("无法备份当前程序（{}）：{error}", backup.display()))?;
    if let Err(error) = fs::copy(new_binary, &current) {
        // 换不上去就把旧文件放回原位，别留下一个启动不了的坑。
        let _ = fs::rename(&backup, &current);
        return Err(format!("无法写入新版本：{error}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&current, fs::Permissions::from_mode(0o755));
    }
    let mut command = Command::new(&current);
    command.arg("--after-update");
    // 脱离当前进程组启动，父进程退出不会把它带走。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("无法启动新版本：{error}"))?;
    // 走到这里必须立刻退出，否则两个实例会抢同一份数据。
    std::process::exit(0);
}

/// 旧版本的备份名：reminder-desktop.old（Windows 保留 .exe 前的部分）。
fn backup_path(current: &Path) -> PathBuf {
    let mut name = current.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
    if let Some(stem) = name.strip_suffix(".exe") {
        name = format!("{stem}.old.exe");
    } else {
        name.push_str(".old");
    }
    current.with_file_name(name)
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

/// 命令行参数用的路径文本。三平台都要用，所以不带 cfg——以前只在 Linux 下定义，
/// macOS 和 Windows 编译会直接失败。
fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧版本的备份名不能把 .exe 弄丢，否则 Windows 上换回来的是一个启动不了的文件。
    #[test]
    fn backup_path_keeps_the_executable_suffix() {
        assert_eq!(
            backup_path(Path::new("/a/b/reminder-desktop")),
            PathBuf::from("/a/b/reminder-desktop.old")
        );
        assert_eq!(
            backup_path(Path::new("/a/b/reminder-desktop.exe")),
            PathBuf::from("/a/b/reminder-desktop.old.exe")
        );
    }

    /// 解压要能穿过包里套的那层目录，把可执行文件找出来。
    #[test]
    fn unpack_finds_the_binary_inside_a_nested_tar_gz() {
        if Command::new("tar").arg("--version").output().is_err() {
            return; // 这台机器没有 tar，跳过
        }
        let root = std::env::temp_dir().join(format!("rd-update-unpack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("pkg/reminder-desktop-v9")).unwrap();
        let inner = root.join("pkg/reminder-desktop-v9/reminder-desktop");
        fs::write(&inner, b"fake binary").unwrap();
        let archive = root.join("reminder-desktop-v9.tar.gz");
        let status = Command::new("tar")
            .args([
                "-czf",
                archive.to_str().unwrap(),
                "-C",
                root.join("pkg").to_str().unwrap(),
                "reminder-desktop-v9",
            ])
            .status()
            .unwrap();
        assert!(status.success(), "造测试用的 tar.gz 失败");

        let found = unpack_binary(&archive, &root.join("out")).unwrap();
        assert_eq!(found.file_name().unwrap(), "reminder-desktop");
        assert_eq!(fs::read(&found).unwrap(), b"fake binary");
        let _ = fs::remove_dir_all(&root);
    }

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
