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
