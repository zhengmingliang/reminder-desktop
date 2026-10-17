#!/usr/bin/env bash
# 打出一个只含原生程序的压缩包。不签名，也不做安装向导。
# Linux / macOS 产出 .tar.gz，Windows 产出 .zip。文件在 dist/。
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
cp "$built" "$stage_parent/$name/$bin_name"
chmod +x "$stage_parent/$name/$bin_name"

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
