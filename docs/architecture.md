# 架构与目录结构

飞花 - PetalDesk 使用 Rust + Tauri 2 提供 Windows 原生能力，Svelte + TypeScript 负责便签、小工具和截图编辑界面。应用正文与用户配置不保存在源码目录中，而是写入用户选择的飞花数据存储目录。

## 主要目录

| 目录 | 内容 |
| --- | --- |
| `src/` | Svelte 页面、组件、编辑器、截图渲染逻辑及前端测试 |
| `src-tauri/` | Rust 命令、存储、窗口、托盘、截图、通知和 NSIS 安装器 |
| `static/` | 应用前端构建时直接复制的静态资源 |
| `website/` | 可独立发布的产品网站、网站图标和演示截图 |
| `docs/` | 架构、发布和维护文档 |
| `scripts/` | Windows 安装包构建与数据迁移脚本 |
| `.github/workflows/` | GitHub Pages 和 Windows Release 自动化 |

`node_modules/`、`.svelte-kit/`、`build/`、`src-tauri/target/` 和测试报告均为可重建内容，通过 `.gitignore` 排除。

## 数据边界

源码仓库不包含真实便签、截图会话、计时记录或提醒数据。用户业务数据统一存放在所选目录的 `.petaldesk/` 下；`%LOCALAPPDATA%\PetalDesk` 仅保留当前数据目录指针、可重建索引和调试日志。

## 本地优先存储

PetalDesk 面向读取频繁、写入较少的桌面使用场景，不为第三方同步盘实现分布式锁、操作日志或 CRDT。数据按类型选择合适粒度：便签使用每条独立的 `note.md + meta.json + assets/`，甘特图使用版本化整体快照，MFA 使用整体认证加密保险库。MFA 保险库的随机数据密钥由 DPAPI 和 Argon2id 恢复密码双重包装，实现本机免密解锁，以及跨电脑或 Windows 用户迁移。

三者共享同一可靠保存原则：写同目录临时文件并刷新磁盘，再使用 Windows 原子替换；提交前校验 revision 与内容哈希或磁盘文件指纹；发现外部修改时拒绝覆盖并保留冲突副本。搜索 SQLite 只是本地可重建缓存，不放入用户数据目录，也不参与迁移。

具体数据粒度、参考项目与迁移边界见 [本地存储设计](storage.md)。
