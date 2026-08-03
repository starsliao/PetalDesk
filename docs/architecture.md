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

## 本地优先存储

PetalDesk 面向读取频繁、写入较少的桌面使用场景，不为第三方同步盘实现分布式锁、操作日志或 CRDT。数据按类型选择合适粒度：便签使用每条独立的 `note.md + meta.json + assets/`，甘特图使用版本化整体快照，MFA 使用整体认证加密保险库。MFA 保险库的随机数据密钥由恢复密码和平台本机保护分别包装：Windows 使用 DPAPI，macOS 使用 Keychain，从而兼顾本机免密解锁，以及跨电脑、系统用户或平台迁移。保险库可以同时保留两种平台包装，任一平台写入都不会丢弃另一平台的本机保护元数据。

三者共享同一可靠保存原则：写同目录临时文件并刷新磁盘，再使用当前平台的原子替换；提交前校验 revision 与内容哈希或磁盘文件指纹；发现外部修改时拒绝覆盖并保留冲突副本。搜索 SQLite 只是本地可重建缓存，不放入用户数据目录，也不参与迁移。

## 平台边界

便签、甘特图、计时器、提醒和系统通知在 Windows 与 macOS 共享同一业务层。普通截图也同时支持两个平台：Windows 使用现有 Win32 捕获链路，macOS 使用系统屏幕捕获并依赖“屏幕录制”权限；编辑、复制、保存和贴图沿用共享流程。

Windows 继续保留长截图、自动滚动、浏览器扩展和 Native Messaging Host。macOS 当前版本不打包或注册 Native Messaging Host，也不提供长截图和浏览器联动，能力查询会让前端隐藏或禁用相应入口。平台代码通过 Rust 条件编译和独立 Tauri 配置隔离，macOS DMG 的加入不会改变 Windows NSIS、DPAPI、Win32 截图与浏览器增强链路。

macOS Release 使用 `universal-apple-darwin` 目标，把 `x86_64-apple-darwin` 与 `aarch64-apple-darwin` 合并为一个 Universal 应用和 DMG。Intel 与 Apple Silicon 用户下载同一个文件即可。

具体数据粒度、参考项目与迁移边界见 [本地存储设计](storage.md)。
