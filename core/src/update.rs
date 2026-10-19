//! 检查更新的纯逻辑：解析更新清单、比较版本号、校验下载文件的 sha256。
//! 网络请求和进程操作留在界面层，这里只做可单测的部分。

use std::cmp::Ordering;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 更新清单（latest.json）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub url: String,
    #[serde(default)]
    pub sha256: String,
}

/// 解析更新清单；缺 version 或 url 视为无效。
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|error| format!("更新清单解析失败：{error}"))?;
    if manifest.version.trim().is_empty() {
        return Err("更新清单缺少 version".into());
    }
    if manifest.url.trim().is_empty() {
        return Err("更新清单缺少 url".into());
    }
    Ok(manifest)
}

/// `candidate` 是否比 `current` 新。按点分段比数字，段数不同按缺省 0 处理。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    compare_versions(candidate, current) == Ordering::Greater
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut left = a.trim().split('.');
    let mut right = b.trim().split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (l, r) => match segment(l).cmp(&segment(r)) {
                Ordering::Equal => continue,
                other => return other,
            },
        }
    }
}

/// 取一段开头的数字；忽略 `-beta` 之类的后缀，取不到就是 0。
fn segment(part: Option<&str>) -> u64 {
    let Some(part) = part else { return 0 };
    let digits: String = part.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

/// 算字节的 sha256，返回小写十六进制。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 校验字节的 sha256 是否与清单一致；清单没给 sha256 就拒绝。
pub fn verify_sha256_bytes(bytes: &[u8], expected: &str) -> Result<(), String> {
    let expected = expected.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return Err("更新清单没有提供 sha256，拒绝使用该下载文件".into());
    }
    let actual = sha256_hex(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(format!("sha256 校验不匹配：期望 {expected}，实际 {actual}"))
    }
}

/// 校验文件 sha256 是否与清单一致。
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    verify_sha256_bytes(&std::fs::read(path).map_err(|error| format!("读取下载文件失败：{error}"))?, expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_manifest_and_rejects_missing_fields() {
        let good = r#"{"version":"1.1.0","notes":"修了些问题","url":"https://e/x.tar.gz","sha256":"abc"}"#;
        let manifest = parse_manifest(good.as_bytes()).unwrap();
        assert_eq!(manifest.version, "1.1.0");
        assert_eq!(manifest.sha256, "abc");

        assert!(parse_manifest(br#"{"url":"https://e/x"}"#).is_err());
        assert!(parse_manifest(br#"{"version":"1.1.0"}"#).is_err());
        assert!(parse_manifest(b"not json").is_err());
    }

    #[test]
    fn compares_versions_numerically_not_lexically() {
        assert!(is_newer("1.1.0", "1.0.0"));
        assert!(is_newer("1.0.10", "1.0.9")); // 按数字比，不是按字符串
        assert!(is_newer("1.1", "1.0.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.1"));
        assert!(!is_newer("1.0.0", "1.0.0.1"));
        assert!(is_newer("1.0.0.1", "1.0.0"));
        assert!(is_newer("1.2.0-beta", "1.1.0"));
    }

    #[test]
    fn sha256_matches_the_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_sha256_accepts_match_and_rejects_mismatch_or_empty() {
        let dir = std::env::temp_dir().join(format!("rd-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("payload.bin");
        std::fs::write(&file, b"abc").unwrap();

        let expected = sha256_hex(b"abc");
        assert!(verify_sha256(&file, &expected.to_uppercase()).is_ok());
        assert!(verify_sha256(&file, "deadbeef").is_err());
        assert!(verify_sha256(&file, "  ").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
