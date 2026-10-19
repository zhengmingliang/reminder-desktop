#!/usr/bin/env bash
# 打出一个只含原生程序的压缩包，不做安装向导。
# Linux / macOS 产出 .tar.gz，Windows 产出 .zip。文件在 dist/。
# 可选签名与公证：macOS 给了 MACOS_SIGN_IDENTITY 就用 Developer ID 签名（再给公证凭据则
# 一并公证并装订票据，产物是 .app）；Windows 给了 WINDOWS_CERT_PFX_BASE64 就用 signtool 签名。
# 没给凭据就照旧出未签名包。
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

version=$(awk -F '"' '/^version = / { print $2; exit }' app/Cargo.toml)
if [[ -z "$version" ]]; then
    echo "无法从 app/Cargo.toml 读到版本号" >&2
    exit 1
fi

arch=$(uname -m)
case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *)
        echo "不支持的架构：$arch" >&2
        exit 1
        ;;
esac

case "$(uname -s)" in
    Linux)
        os_name=linux
        ext=tar.gz
        bin_name=reminder-desktop
        ;;
    Darwin)
        os_name=macos
        ext=tar.gz
        bin_name=reminder-desktop
        ;;
    MINGW* | MSYS* | CYGWIN*)
        os_name=windows
        ext=zip
        bin_name=reminder-desktop.exe
        ;;
    *)
        echo "不支持的系统：$(uname -s)" >&2
        exit 1
        ;;
esac

# 可选签名与公证：只有在提供对应凭据时才走签名流程，否则照旧出未签名包。
macos_sign=false
windows_sign=false
if [[ "$os_name" == macos && -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
    macos_sign=true
fi
if [[ "$os_name" == windows && -n "${WINDOWS_CERT_PFX_BASE64:-}" ]]; then
    windows_sign=true
fi

# 写一份最小可用的 macOS .app 的 Info.plist（签名/公证需要正规的 bundle 结构）。
write_info_plist() {
    local plist="$1"
    local bundle_id="${MACOS_BUNDLE_ID:-com.alianga.reminder-desktop}"
    cat >"$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>桌面提醒</string>
    <key>CFBundleDisplayName</key>
    <string>桌面提醒</string>
    <key>CFBundleIdentifier</key>
    <string>${bundle_id}</string>
    <key>CFBundleExecutable</key>
    <string>reminder-desktop</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>${MACOS_MIN_VERSION:-11.0}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST
}

# 用 Developer ID 证书签名；若还给了公证凭据则接着提交公证并装订票据。
sign_and_notarize() {
    local app="$1"
    echo "用 ${MACOS_SIGN_IDENTITY} 签名 $(basename "$app")"
    codesign --force --deep --options runtime --timestamp \
        --sign "$MACOS_SIGN_IDENTITY" "$app"
    codesign --verify --strict --verbose=2 "$app"

    if [[ -z "${MACOS_NOTARY_APPLE_ID:-}" && -z "${MACOS_NOTARY_API_KEY:-}" ]]; then
        echo "未提供公证凭据（MACOS_NOTARY_APPLE_ID 或 MACOS_NOTARY_API_KEY），只签名不公证"
        return 0
    fi

    local work notarize_zip
    work=$(mktemp -d)
    notarize_zip="$work/notarize.zip"
    ditto -c -k --keepParent "$app" "$notarize_zip"
    echo "提交公证（notarytool，等待结果）"
    if [[ -n "${MACOS_NOTARY_API_KEY:-}" ]]; then
        xcrun notarytool submit "$notarize_zip" --wait \
            --key "$MACOS_NOTARY_API_KEY" \
            --key-id "${MACOS_NOTARY_KEY_ID:?需要 MACOS_NOTARY_KEY_ID}" \
            --issuer "${MACOS_NOTARY_ISSUER:?需要 MACOS_NOTARY_ISSUER}"
    else
        xcrun notarytool submit "$notarize_zip" --wait \
            --apple-id "$MACOS_NOTARY_APPLE_ID" \
            --team-id "${MACOS_NOTARY_TEAM_ID:?需要 MACOS_NOTARY_TEAM_ID}" \
            --password "${MACOS_NOTARY_PASSWORD:?需要 MACOS_NOTARY_PASSWORD}"
    fi
    echo "装订公证票据"
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    rm -rf "$work"
}

# 用 signtool 给 exe 签名并校验。
sign_windows_exe() {
    local exe="$1"
    local work cert
    work=$(mktemp -d)
    cert="$work/codesign.pfx"
    printf '%s' "$WINDOWS_CERT_PFX_BASE64" | base64 --decode >"$cert"
    echo "用 signtool 签名 $exe"
    signtool sign /fd SHA256 /f "$cert" /p "$WINDOWS_CERT_PASSWORD" \
        /tr http://timestamp.digicert.com /td SHA256 "$exe"
    signtool verify /pa "$exe"
    rm -rf "$work"
}

label="${os_name}-${arch}"
name="reminder-desktop-${version}-${label}"
archive="$root/dist/${name}.${ext}"
checksum="$archive.sha256"

if command -v python3 >/dev/null 2>&1; then
    py=python3
elif command -v python >/dev/null 2>&1; then
    py=python
else
    echo "需要 python3 来写压缩包和校验和" >&2
    exit 1
fi

echo "构建 ${name}"
cargo build --release -p reminder-desktop

built="target/release/${bin_name}"
if [[ ! -f "$built" ]]; then
    echo "没有找到 $built" >&2
    exit 1
fi

echo "运行 --check"
"$built" --check

stage_parent=$(mktemp -d)
trap 'rm -rf "$stage_parent"' EXIT
mkdir -p "$stage_parent/$name"

if $macos_sign; then
    # 签名与公证要求正规的 .app 结构，所以这里改成打 .app。
    app="$stage_parent/$name/reminder-desktop.app"
    mkdir -p "$app/Contents/MacOS"
    cp "$built" "$app/Contents/MacOS/reminder-desktop"
    chmod +x "$app/Contents/MacOS/reminder-desktop"
    write_info_plist "$app/Contents/Info.plist"
    sign_and_notarize "$app"
else
    cp "$built" "$stage_parent/$name/$bin_name"
    chmod +x "$stage_parent/$name/$bin_name"
    if $windows_sign; then
        sign_windows_exe "$stage_parent/$name/$bin_name"
    fi
fi

mkdir -p "$root/dist"
rm -f "$archive" "$checksum"
"$py" - "$archive" "$stage_parent" "$name" <<'PY'
import os
import sys
import tarfile
import zipfile

archive, parent, name = sys.argv[1:]
src = os.path.join(parent, name)
if archive.endswith(".zip"):
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
        for dirpath, _, files in os.walk(src):
            for filename in files:
                full = os.path.join(dirpath, filename)
                bundle.write(full, os.path.relpath(full, parent))
else:
    with tarfile.open(archive, "w:gz") as bundle:
        bundle.add(src, arcname=name)
PY

"$py" - "$archive" "$checksum" <<'PY'
import hashlib
import os
import sys

archive, checksum = sys.argv[1:]
digest = hashlib.sha256(open(archive, "rb").read()).hexdigest()
line = f"{digest}  {os.path.basename(archive)}\n"
open(checksum, "w", encoding="utf-8").write(line)
print(line, end="")
PY

# 用仓库内的相对路径。Windows 的 bash 会写出 /d/... 这种路径，上传动作认不出来。
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    {
        echo "archive=dist/${name}.${ext}"
        echo "sha=dist/${name}.${ext}.sha256"
        echo "label=$name"
    } >>"$GITHUB_OUTPUT"
fi

echo "已打包：$archive"
