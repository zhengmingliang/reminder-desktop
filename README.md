# 桌面提醒

跨平台桌面提醒。安装包是一颗原生程序，不打包浏览器，也不带一套 Java 运行时。Windows、macOS、Linux 共用同一套调度和界面。

界面用 [Slint](https://slint.dev)（Fluent 风格，软件渲染）。提醒规则、校验、到期计算和磁盘存储在 `core/`，不依赖窗口，可以用 `cargo test -p reminder-core` 单独跑。

## 能做什么

- 标题和正文
- 三种时间：若干分钟后、指定到某一分钟、5 段 Cron（分 时 日 月 周）
- Cron 输入时在一块矮列表里预览最近 10 次，可用滚轮滚动
- 三种输入都会在字段下给出具体错误
- 列表里可改、可删。已经触发过的提醒改时间后再保存，会重新启用
- 数据写在本机数据目录，原子替换并保留上一份 `.bak`。主文件损坏时从备份恢复；版本比程序新时拒绝覆盖
- 关掉主窗口后进程继续跑，到点仍会弹出提醒。Linux Mint（Cinnamon）用系统 XEmbed 托盘，左键重新打开主窗口，右键是「显示主窗口 / 退出」。KDE 等提供 StatusNotifier 的桌面走另一套托盘。再启动一次程序也会把窗口叫回来
- 顶栏右上角有设置按钮（齿轮），可在浅色/跟随系统/深色之间切换主题（默认跟随系统，选择存在数据目录的 settings.json）、开关开机自启动、从备份恢复；托盘菜单的「设置…」是同一入口
- 弹窗在光标所在的显示器正中弹出（纯 Wayland 会话拿不到全局光标，退回系统主屏），不自动消失。可以选 5 分钟、15 分钟、30 分钟或 1 小时后再响一次
- 纯 Wayland 会话里弹窗的定位与置顶不一定被合成器尊重，因此到期时在弹窗之外再发一条系统通知（Linux 走 `notify-send`）兜底；X11 下只弹窗。排查时可用环境变量 `REMINDER_SYSTEM_NOTIFY=1` 在任意会话强制开启
- 每条提醒可以开关铃声。不选文件时用内置提示音，也可以换成 wav、mp3、ogg、flac（15 MB 以内）
- 列表、表单、Cron 预览、文件选择和弹窗正文都可以用滚轮滚动。输入框支持 Ctrl+A 全选（macOS 上全选是 Command+A）
- 可以在命令行直接添加：`reminder add 喝水 --in 5m`（也支持 `--at 09:30`、`--cron "0 9 * * 1-5"`、`--body 正文`）。界面没开就直接写库；界面在跑则把提醒交给它处理，稍后即可在列表里看到
- 配置了更新地址就自动查更新：启动后与每 6 小时各查一次，发现新版本在设置面板提示，点「下载更新」拉包并校验 sha256，校验通过才落盘并打开所在目录，替换由用户手动完成

## 检查更新

在数据目录的 `settings.json` 里填 `update_url`（也可用环境变量 `REMINDER_UPDATE_URL` 覆盖，主要供本地验证）：

```json
{ "theme": "system", "update_url": "https://example.com/reminder-desktop/latest.json" }
```

清单格式：

```json
{ "version": "1.1.0", "notes": "修了哪些问题", "url": "https://…/reminder-desktop-v1.1.0.tar.gz", "sha256": "…" }
```

版本按点分段比数字（`1.0.10` 大于 `1.0.9`）。下载会先校验 sha256 再写盘，清单没给 sha256 或对不上就拒绝，文件不落地。程序只负责「检查 + 提示 + 下载校验」，不自动替换当前二进制。

## 命令行

```bash
reminder add 喝水 --in 5m            # 5 分钟后（也可用 1h、2d、45）
reminder add 站会 --at 09:30          # 今天 09:30，已过则顺延到明天
reminder add 每日汇总 --cron "0 18 * * 1-5" --body "写日报"
```

## 构建

需要 Rust 1.92 或更新。

在项目根目录执行：

```bash
cargo test -p reminder-core
cargo build --release -p reminder-desktop
./target/release/reminder-desktop
```

Linux 运行时需要 `libfontconfig`、`libasound`、`libxkbcommon`、`libxcb`。编译时如果没有 `fontconfig.pc`，本目录的 `.cargo/config.toml` 会使用 `packaging/pkgconfig/fontconfig.pc`。有开发包时也可以直接安装 `libfontconfig1-dev`。

Linux Mint Cinnamon 的面板是传统系统托盘，不提供 `org.kde.StatusNotifierWatcher`。程序在检测到 `_NET_SYSTEM_TRAY_S0` 时嵌入 XEmbed 图标。没有这块托盘时才退回 StatusNotifier。纯 Wayland 会话两种都没有，关掉窗口后只能再次运行程序把它叫回来。

和成熟企业通知产品的差距写在 `docs/enterprise-gap.md`。

Windows 发布构建不弹控制台。macOS 的菜单栏图标由 Slint 的状态项实现。

## 签名与公证（可选）

`packaging/package.sh` 默认出未签名的包；只有提供了凭据才会签名，凭据缺失就自动跳过。CI（`.github/workflows/package.yml`）通过仓库 Secrets 注入，未配置的 secret 会展开成空串，等同跳过。

macOS：配置 `MACOS_SIGN_IDENTITY`（Developer ID 证书名）即用 `codesign` 签名，并改打成 `reminder-desktop.app`；再配置公证凭据则接着 `notarytool submit --wait` 并 `stapler` 装订票据。公证凭据二选一：

- Apple ID：`MACOS_NOTARY_APPLE_ID`、`MACOS_NOTARY_TEAM_ID`、`MACOS_NOTARY_PASSWORD`（app 专用密码）
- API Key：`MACOS_NOTARY_API_KEY`（.p8 路径）、`MACOS_NOTARY_KEY_ID`、`MACOS_NOTARY_ISSUER`

可选用 `MACOS_BUNDLE_ID` 覆盖 bundle id（默认 `com.alianga.reminder-desktop`）。

Windows：配置 `WINDOWS_CERT_PFX_BASE64`（.pfx 的 Base64）与 `WINDOWS_CERT_PASSWORD` 即用 `signtool` 签名并校验。

## 数据目录

| 系统 | 位置 |
| --- | --- |
| Linux | `$XDG_DATA_HOME/reminder-desktop`，否则 `~/.local/share/reminder-desktop` |
| macOS | `~/Library/Application Support/ReminderDesktop` |
| Windows | `%APPDATA%\ReminderDesktop` |

`reminders.json` 是提醒，`reminders.json.bak` 是上一份，`app.log` 是运行日志。

## 时间规则

- 延迟从保存或重新启用的那一刻起算，精确到秒
- 指定时间必须晚于现在，精确到分钟
- Cron 不写秒。日和周都写了限制时，满足其中一个就触发
- 保存 Cron 后从保存时刻之后的下一次开始，不会把创建之前的周期补出来
- 程序没开的时候错过的周期，重新打开后合并成一条弹窗，并写明错过了多少次

开发自检：`reminder-desktop --check` 不打开窗口。`--smoke` 打开窗口后马上退出。

## 打包

在项目根目录执行：

```bash
./packaging/package.sh
```

脚本会做 release 构建、跑一遍 `--check`，再把单个程序放进 `dist/`。Linux 和 macOS 是 `.tar.gz`，Windows 是 `.zip`。包没有签名，也没有安装向导。

推送 `v*` 标签，或在 GitHub Actions 里手动运行 Package，会打出三个包：Linux x86_64、macOS Apple 芯片（aarch64）、Windows x86_64。标签推送还会把这些包挂到对应的 GitHub Release。
