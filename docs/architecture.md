# 架构与目录结构

飞花 - PetalDesk 使用 Rust + Tauri 2 对接 Windows 与 macOS 的窗口、托盘、通知、截图和安全存储能力，Svelte + TypeScript 负责便签、小工具和截图编辑界面。应用正文与用户配置不保存在源码目录中，而是写入用户选择的飞花数据存储目录。

## 主要目录

| 目录 | 内容 |
| --- | --- |
| `src/` | Svelte 页面、组件、编辑器、截图渲染逻辑及前端测试 |
| `src-tauri/` | Rust 命令、存储、窗口、托盘、截图、通知，以及 Windows NSIS / macOS DMG 配置 |
| `static/` | 应用前端构建时直接复制的静态资源 |
| `website/` | 可独立发布的产品网站、网站图标和演示截图 |
| `docs/` | 架构、发布和维护文档 |
| `scripts/` | Windows 安装包构建与数据迁移脚本 |
| `.github/workflows/` | GitHub Pages，以及 Windows x64 与 macOS Universal Release 自动化 |

`node_modules/`、`.svelte-kit/`、`build/`、`src-tauri/target/` 和测试报告均为可重建内容，通过 `.gitignore` 排除。

## 数据边界

源码仓库不包含真实便签、截图会话、计时记录或提醒数据。用户业务数据统一存放在所选目录的 `.petaldesk/` 下；Windows 的 `%LOCALAPPDATA%\PetalDesk` 或 macOS 的 `~/Library/Application Support/PetalDesk` 仅保留当前数据目录指针、可重建索引和调试日志。

桌面诊断日志固定写入上述本机应用目录的 `logs/petaldesk-diagnostics.log`，不跟随用户选择的数据目录，避免同步盘或网络盘阻塞事件线程。日志只在单实例所有权确认后打开；一个独立写线程负责有界队列、JSONL 序列化与 1 MiB x 4 文件轮转。独立 watchdog 通过单个未决主线程探针检测事件循环停顿，Windows 等待链则交给唯一的隔离线程采集；查询超过 5 秒会留下超时记录，但不会停止后续 watchdog、恢复事件或进程资源采样。事件名和字段采用封闭的静态集合，不接收便签、路径、网址、账户、密码、MFA、剪贴板或浏览器消息内容。

## 本地优先存储

PetalDesk 面向读取频繁、写入较少的桌面使用场景，不为第三方同步盘实现分布式锁、操作日志或 CRDT。数据按类型选择合适粒度：便签使用每条独立的 `note.md + meta.json + assets/`，甘特图使用版本化整体快照，MFA 与密码管理器分别使用独立的认证加密保险库。每个保险库拥有独立随机数据密钥，由恢复密码和平台本机保护分别包装；共享恢复密码协调器负责首次启用和轮换时保持两库一致。Windows 使用 DPAPI，macOS MFA 使用 Keychain，从而兼顾本机免密解锁，以及跨电脑、系统用户或平台迁移。

这些数据共享同一可靠保存原则：写同目录临时文件并刷新磁盘，再使用当前平台的原子替换；提交前校验 revision 与内容哈希或磁盘文件指纹；发现外部修改时拒绝覆盖并保留冲突副本。搜索 SQLite 只是本地可重建缓存，不放入用户数据目录，也不参与迁移。

## 平台边界

便签、甘特图、计时器、提醒和系统通知在 Windows 与 macOS 共享同一业务层。普通截图也同时支持两个平台：Windows 使用现有 Win32 捕获链路，macOS 使用系统屏幕捕获并依赖“屏幕录制”权限；编辑、复制、保存和贴图沿用共享流程。

Windows 继续保留长截图、自动滚动、浏览器扩展和 Native Messaging Host，并在 `0.6.0` 增加密码管理器。截图控制仍使用原有文件桥接；用户名、密码和登录候选只经过带当前用户 ACL、随机进程令牌和短时会话绑定的 Windows named pipe，不进入文件队列。密码填充、登录保存提示和关联 TOTP 由 Firefox、Chrome 与 Edge 共享同一套扩展协议，页面确认后只填写字段、不自动提交。macOS 当前版本不打包或注册 Native Messaging Host，也不提供密码管理器、长截图和浏览器联动。

网络体检同样是 Windows 独有的小工具。采集走 `GetExtendedTcpTable`，而不是 netstat 子进程或 `Get-NetTCPConnection`：后两者在两万条连接的量级上分别需要数百毫秒到数秒，且 `Get-NetTCPConnection` 会直接超时返回空表 —— 恰恰是连接数异常、最需要体检的时候不可用。动态端口范围没有公开的 Win32 查询接口，只能调用一次 `netsh` 并按整数提取，失败时回退到系统默认范围。采集与聚合都在 `run_background` 的工作线程上完成，聚合后只把百余行的报表送过 IPC，不把原始连接表交给前端。聚合逻辑是与平台无关的纯函数，因此可以脱离 Windows API 单测；工具只读取连接元数据，不读取任何流量内容，也不写入诊断日志。

macOS Release 使用 `universal-apple-darwin` 目标，把 `x86_64-apple-darwin` 与 `aarch64-apple-darwin` 合并为一个 Universal 应用和 DMG。Intel 与 Apple Silicon 用户下载同一个文件即可。

具体数据粒度、参考项目与迁移边界见 [本地存储设计](storage.md)。
