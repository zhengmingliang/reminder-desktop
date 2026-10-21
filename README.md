# 桌面提醒

跨平台桌面提醒。安装包是一颗原生程序，不打包浏览器，也不带一套 Java 运行时。Windows、macOS、Linux 共用同一套调度和界面。

界面用 [Slint](https://slint.dev)（Fluent 风格，软件渲染）。提醒规则、校验、到期计算和磁盘存储在 `core/`，不依赖窗口，可以用 `cargo test -p reminder-core` 单独跑。

## 能做什么

### 提醒本身

- 标题和正文
- 三种时间：若干分钟后、指定到某一分钟、5 段 Cron（分 时 日 月 周）；日字段支持 `L` 表示每月最后一天（预设里有「每月最后一天」）
- Cron 可设结束日期（到那天 23:59 后不再触发）和最多触发次数（到次数后停止），都留空就一直循环
- 循环提醒可「跳过下一次」：编辑视图底部点一下，到点那一次不弹也不计次数，之后照常；再点「取消跳过」可撤回
- Cron 输入时在一块矮列表里预览最近 10 次，可用滚轮滚动
- 三种输入都会在字段下给出具体错误
- 可以提前提醒：表单「弹窗与铃声」里选 5 分钟 / 10 分钟 / 30 分钟 / 1 小时（默认不提前），到点前先弹一次写着「这是提前提醒」的提示，到点后照常再弹正式提醒；命令行 `reminder add 吃药 --at 21:00 --early 10m` 也可设置。每个触发点只提前一次，暂停或稍后提醒期间不提前
- 弹窗在光标所在的显示器正中弹出（纯 Wayland 会话拿不到全局光标，退回系统主屏），不自动消失；正文过长时内容框会自适应加高（约 6 行封顶），再长可在框内滚动
- 弹窗一直没被「知道了」或稍后提醒处理时，会按设定间隔再响一次并重新置顶：设置面板里可选关 / 每 5 分钟 / 每 10 分钟 / 每 30 分钟（默认 5 分钟），一旦确认或稍后提醒就不再响
- 每条提醒可以开关铃声。不选文件时用内置提示音，也可以换成 wav、mp3、ogg、flac（15 MB 以内）
- 勿扰时段（默认关）：设置里可设起止时间（支持跨午夜，如 22:00-07:00）和模式「静音弹窗 / 只记录」。静音弹窗照弹但不响铃不发系统通知；只记录则不弹不响、提醒正常推进、记入概览「补看」列表，勿扰结束后可补看
- 补发过的提醒会在概览顶部留一条提示（危险色横幅），点「补看」把这几条再弹一遍，点「知道了」收起
- 纯 Wayland 会话里弹窗的定位与置顶不一定被合成器尊重，因此到期时在弹窗之外再发一条系统通知（Linux 走 `notify-send`）兜底；X11 下只弹窗。排查时可用环境变量 `REMINDER_SYSTEM_NOTIFY=1` 在任意会话强制开启
- X11 下弹窗会被标成「所有工作区 + 最上层」的窗口，并主动请求窗口管理器激活，因此全屏应用或切到别的工作区都不会把它埋掉（纯 Wayland 没有这套 EWMH 协议，仍走系统通知兜底）

### 列表与整理

- 概览页四个数字卡片：待触发 / 今天已触发 / 今天已完成 / 今天已错过
- 列表可切换排列方式：按状态分组（即将触发 / 已触发 / 已完成 / 已停用），或按时间分档（今天 / 明天 / 本周 / 更远）。点分组标题只看该组，再点一次恢复全部。选择记在 `settings.json`
- 区分「已确认」和「被关掉」：弹窗点「知道了」记成「已完成」，直接关掉弹窗只记「已触发」，列表里一眼能看出哪些提醒真被处理过
- 标签：表单里随手写一个分类名（最长 12 字，可留空）。颜色由标签文本算出，同名永远同色；列表行左侧有对应色条，列表上方可以按标签筛选
- `Ctrl+F` 搜索标题、正文、标签或 Cron 表达式
- 多选：Ctrl 点选、Shift 范围选，可批量移入回收站或彻底删除
- 回收站：删掉的提醒先进回收站，可单条恢复或彻底删除。保留期在设置里可调（1-365 天，默认 30 天），过期自动清除
- 列表里可改、可删。已经触发过的提醒改时间后再保存，会重新启用

### 数据与备份

- 数据写在本机数据目录，原子替换并保留上一份 `.bak`；每次启动还会留一份按天的快照（`reminders-YYYYMMDD.json`），两者加起来的备份最多保留 10 份，可在设置里「从备份恢复」。主文件损坏时先从 `.bak` 恢复并明确提示；主文件和备份都坏就隔离原文件并暂停写入，版本比程序新时同样拒绝覆盖
- 设置面板可「导出 JSON」「导出日历 .ics」「导入」。导出文件落在数据目录的 `export/` 下、文件名带时间戳，导完顺手打开所在目录。`.ics` 只导出启用中、还能算出下次触发时间的条目（DTSTART 是本地浮动时间，Cron 会写进备注），可直接喂给系统日历
- 导入是合并而不是覆盖：文件里 id 已存在、或已在回收站的条目会跳过；循环提醒从导入时刻往后排，原来那台机器上的稍后 / 跳过 / 提前记录一律清掉
- 设置面板可「打开数据目录」，直接用系统文件管理器翻备份和日志

### 托盘、窗口与更新

- 关掉主窗口后进程继续跑，到点仍会弹出提醒。点关闭按钮默认隐藏到托盘、后台继续提醒（可在设置里改成直接退出）；要彻底退出用设置面板底部的「退出程序」
- 托盘左键切换主窗口，右键菜单是「打开主窗口 / 新建提醒… / 备份… / 设置… / 退出」。Windows / macOS 走系统原生托盘，Linux 走 XEmbed 还是 StatusNotifier 见下面「Linux 托盘」
- 最小化按钮在有托盘图标时最小化到托盘（窗口隐藏、图标保留，左键唤回）；托盘不可用时退回任务栏最小化，窗口不会藏进叫不回来的状态
- 再启动一次程序也会把窗口叫回来（同时只有一个实例在跑，第二个进程只负责请求第一个把窗口打开然后退出），窗口处于最小化时会一并恢复出来。没有托盘图标的会话里，这是唯一的唤回方式
- 顶栏右上角只有设置按钮（齿轮），设置面板集中了其余入口：主题（浅色/跟随系统/深色，默认跟随系统，选择存在数据目录的 settings.json）、开关开机自启动、勿扰时段、全局快捷键、关闭按钮行为、回收站保留期、立即备份与从备份恢复、导入与导出、数据与日志、检查更新、完全退出
- X11 会话可开全局快捷键（默认开，设置里可关）。两个组合键都能在设置里改：写成 `Ctrl+Alt+R` 这样，修饰键是 Ctrl / Alt / Shift / Win（也认 Control、Super、Meta），主键可以是单个字母数字、`F1`-`F12`，或 Space、Enter、Tab、Esc、Home、End、PageUp、PageDown、Insert、Delete 这些命名键。必须带至少一个修饰键，否则会把普通按键整个抓走。改完立刻生效；解析不出来就保留原值并在字段下提示，「恢复默认」回到 `Ctrl+Alt+R`（呼出/隐藏主窗口）和 `Ctrl+Alt+N`（快速新建）。快捷键被别的程序占用或纯 Wayland 会话会在设置里提示不可用
- 配置了更新地址就自动查更新：启动后与每 6 小时各查一次，发现新版本在设置面板提示，点「下载更新」拉包并校验 sha256，校验通过才落盘，随后可点「安装并重启」由程序自己替换：先把当前程序另存为 `.old` 再换上新版本并重新拉起。替换失败会提示手动换回 `.old`
- 设置面板可「查看日志」：把 `app.log` 的尾部直接贴在面板里，排查问题不用去翻文件
- 列表、表单、Cron 预览、文件选择和弹窗正文都可以用滚轮滚动。输入框支持 Ctrl+A 全选（macOS 上全选是 Command+A）。Esc 关掉设置面板或文件选择
- 可以在命令行直接添加：`reminder add 喝水 --in 5m`（也支持 `--at 09:30`、`--cron "0 9 * * 1-5"`、`--body 正文`）。界面没开就直接写库；界面在跑则把提醒交给它处理，稍后即可在列表里看到

## 检查更新

在数据目录的 `settings.json` 里填 `update_url`（也可用环境变量 `REMINDER_UPDATE_URL` 覆盖，主要供本地验证）：

```json
{ "theme": "system", "update_url": "https://example.com/reminder-desktop/latest.json" }
```

清单格式：

```json
{ "version": "1.1.0", "notes": "修了哪些问题", "url": "https://…/reminder-desktop-v1.1.0.tar.gz", "sha256": "…" }
```

版本按点分段比数字（`1.0.10` 大于 `1.0.9`）。下载会先校验 sha256 再写盘，清单没给 sha256 或对不上就拒绝，文件不落地。程序只负责「检查 + 提示 + 下载校验」，替换与否由用户决定：点「安装并重启」才会替换当前二进制并重新拉起。

## 命令行

```bash
reminder add 喝水 --in 5m            # 5 分钟后（也可用 1h、2d、45）
reminder add 站会 --at 09:30          # 今天 09:30，已过则顺延到明天
reminder add 每日汇总 --cron "0 18 * * 1-5" --body "写日报"
```

## Linux 托盘

托盘分两条路，程序自己判断走哪条，设置面板里会写明当前落在哪条路上：

- **XEmbed**：检测到 `_NET_SYSTEM_TRAY_S0` 时把图标嵌进面板（Linux Mint Cinnamon、Xfce、MATE 这类传统托盘）。
- **StatusNotifierItem**：X11 会话里没有 XEmbed 托盘时退回这条（KDE、装了 AppIndicator 扩展的 GNOME）。

纯 Wayland 会话没有 X 服务器，XEmbed 直接跳过，只走 StatusNotifier。而 StatusNotifier 需要会话总线上有 `org.kde.StatusNotifierWatcher` 宿主（KDE Plasma、waybar 会提供；裸 wlroots 合成器默认没有）。Slint 内部会吞掉 ksni 的注册失败，所以程序在建完托盘后会回查总线确认图标真的挂上去了；没挂上就每 20 秒重试一次、最多试 10 分钟，因此先启动程序、后拉起 waybar 或装 GNOME 扩展也能补上图标。

会话里始终没有托盘宿主时，隐藏起来的窗口没有图标可以叫回来，这时再次运行 `reminder-desktop` 会通过单实例机制唤回主窗口（提醒不受影响，调度在进程内照常跑）。

## Windows / macOS 托盘

两个系统都走 Slint 的原生托盘后端，注册是同步的：创建成功即图标已挂上；创建失败会按 20 秒一次、最多 10 分钟的节奏重试，期间最小化按钮退回任务栏最小化、设置面板会写明现状，再次运行程序也能通过单实例机制唤回窗口。

Windows 上是 `Shell_NotifyIconW` 通知区图标（Windows 10 和 11 同一套接口）：左键切换主窗口显隐，右键弹出菜单；资源管理器重启后图标会借 `TaskbarCreated` 广播自动重新注册，不会丢。macOS 上是菜单栏右侧的状态项，左键同样切换主窗口。

## 构建

需要 Rust 1.92 或更新。

在项目根目录执行：

```bash
cargo test -p reminder-core
cargo build --release -p reminder-desktop
./target/release/reminder-desktop
```

Linux 运行时需要 `libfontconfig`、`libasound`、`libxkbcommon`、`libxcb`。编译时如果没有 `fontconfig.pc`，本目录的 `.cargo/config.toml` 会使用 `packaging/pkgconfig/fontconfig.pc`。有开发包时也可以直接安装 `libfontconfig1-dev`。

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

- `reminders.json`：提醒本体
- `reminders.json.bak`：上一份；`reminders-YYYYMMDD.json`：按天的快照（最多 10 份，含 `.bak`）
- `settings.json`：主题、勿扰、快捷键、排列方式、回收站保留期、更新地址等
- `app.log`：运行日志（超过 1 MB 轮转成 `app.log.1`）
- `export/`：导出的 JSON 与 `.ics`
- `updates/`：下载下来的更新包
- `instance.lock`：单实例锁；`show.request`：第二个实例留下的「请把窗口打开」请求

## 时间规则

- 延迟从保存或重新启用的那一刻起算，精确到秒
- 指定时间必须晚于现在，精确到分钟
- Cron 不写秒。日和周都写了限制时，满足其中一个就触发
- 保存 Cron 后从保存时刻之后的下一次开始，不会把创建之前的周期补出来
- 程序没开的时候错过的周期，重新打开后合并成一条弹窗，并写明错过了多少次；睡眠唤醒、改系统时间、换时区后同样在 5 秒内补发

开发自检：`reminder-desktop --check` 不打开窗口，做一遍自检就退出。`--smoke` 打开窗口后马上退出。`--after-update` 是更新替换流程内部用的（旧进程拉起新进程时多等一会儿再判断有没有别的实例），一般不用手动传。

## 打包

在项目根目录执行：

```bash
./packaging/package.sh
```

脚本会做 release 构建、跑一遍 `--check`，再把单个程序放进 `dist/`。Linux 和 macOS 是 `.tar.gz`，Windows 是 `.zip`。包没有签名，也没有安装向导。

推送 `v*` 标签，或在 GitHub Actions 里手动运行 Package，会打出三个包：Linux x86_64、macOS Apple 芯片（aarch64）、Windows x86_64。标签推送还会把这些包挂到对应的 GitHub Release。
