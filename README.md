# 飞花 - PetalDesk

> 把想法留在桌面，把文件留在自己手里。

[产品网站](https://starsliao.github.io/PetalDesk/) · [GitHub](https://github.com/starsliao/PetalDesk) · [下载 Windows 安装包](https://github.com/starsliao/PetalDesk/releases/download/v0.4.0/PetalDesk_0.4.0_x64-setup.exe)

飞花 - PetalDesk 是一款 Windows 10/11 本地便签与效率工具。它启动快、界面安静，支持 Markdown 即时排版、纯文本、图片、搜索、回收站，以及截图、任务规划和本地 MFA 验证器等随时可以唤起的小工具。没有账号、没有云端依赖，内容保留在你的本地数据目录中。

<p align="center">
  <img src="website/assets/screenshots/main-window-0.2.1.png" alt="飞花 - PetalDesk 主界面" width="860" />
</p>

## 一张便签，按你的方式记录

- Markdown 与纯文本两种模式，每张便签可以独立选择；默认样式只影响新建便签。
- Typora 风格的即时排版，支持标题、列表、任务清单、表格、链接、图片、代码、高亮和分隔线；链接可以 `Ctrl+左键` 或右键菜单跳转，表格中也可以使用安全的 `<img src="assets/..." width="500">` 图片标签。
- 标题、颜色、置顶、只读、窗口位置和大小都可以单独保存。
- 便签关闭后继续驻留托盘；双击托盘图标直接打开上次使用的便签，右键可打开列表。
- 删除前二次确认，删除后进入回收站；正文支持撤回、重做和自动保存。

<p align="center">
  <img src="website/assets/screenshots/markdown-note-editor.png" alt="飞花 - PetalDesk Markdown 便签编辑窗口与工具栏" width="520" />
</p>

## 不只是便签

| 任务甘特图 | 截图与长截图 |
| --- | --- |
| <img src="website/assets/screenshots/gantt-tool.png" alt="飞花 - PetalDesk 任务甘特图" width="500" /> | <img src="website/assets/screenshots/screenshot-tool-final.png" alt="飞花 - PetalDesk 截图工具" width="500" /> |

- 计时器：透明电子管数字、暂停/重置记录、透明度、位置和大小记忆。
- 提醒：一次、间隔、日/周/月/年周期，到点发送 Windows 通知。
- 任务甘特图：任务排序、进度筛选、时间条拖动、小时级时间轴缩放。
- MFA 验证器：支持标准 `TOTP` 单账户、屏幕二维码扫描、图片/链接/手动导入、默认隐藏验证码与双击安全复制；本机使用 Windows DPAPI 免密解锁，恢复密码用于跨电脑迁移。
- 截图：默认 `F1`，单显示器手动框选，标注、马赛克、模糊、复制、保存和置顶贴图；选区内双击即可复制，选区外右键直接取消。
- 长截图：默认由用户在原窗口中手动滚动并实时拼接，支持暂停、重试、回退和完整标注；自动滚动作为高级模式保留，浏览器扩展可增强 Chrome、Edge 与 Firefox 长页面的滚动定位和拼接稳定性。

长截图的默认操作只有三步：按 `F1` 框选固定区域并点击工具栏中的长截图按钮；冻结画面切回原窗口后，选区外仍保留暗色遮罩，直接在选区内向下滚动；控制条帧数增长后，点击“完成”。无需再次点击选区，普通 Windows 窗口也不需要安装浏览器扩展。向上滚动只会回看已捕获内容，不会反向写入长图；重新向下越过已捕获末尾后会自动继续拼接。采集会跟随真实滚动连续取帧，并在停止滚动后补一张稳定帧；空闲等待不会自动结束。需要自动滚动时，从长截图按钮旁的小箭头选择自动模式，再点击选区内真正会滚动的正文区域。

## 数据真正属于你

安装时可以选择“飞花 - PetalDesk 数据存储”，默认位置是用户“文档”目录下的 `PetalDesk`。便签、附件、设置和普通小工具数据迁移到新电脑时，复制整个目录，再在安装器或设置中指定它即可。存储按“本地读多、写入很少”设计；坚果云、OneDrive 等同步目录只作为可选的文件搬运方式，不依赖专有同步协议。

```text
PetalDesk/
└─ .petaldesk/
   ├─ notes/<note-id>/
   │  ├─ note.md
   │  ├─ meta.json
   │  └─ assets/
   ├─ config.json
   ├─ state/
   ├─ tools/
   │  ├─ gantt/
   │  │  ├─ gantt.json     # 版本化任务快照
   │  │  ├─ backups/
   │  │  └─ conflicts/
   │  └─ mfa/
   │     ├─ vault.json     # DPAPI + 恢复密码双包装的 AEAD 加密保险库
   │     ├─ backups/
   │     └─ conflicts/
   ├─ backups/
   ├─ journal/
   ├─ trash/
   └─ conflicts/
```

`note.md` 是正文唯一真相，图片使用便签目录内 `assets/` 的相对路径。Markdown 图片和受控的 HTML `<img>` 标签都经过相同的本地资源映射；脚本、事件属性、任意本地路径和远程图片不会被加载。普通读取不会改写笔记；搜索索引按内容哈希增量维护且随时可以重建，不会成为迁移或恢复的前置条件。甘特图不保存为同步服务准备的删除墓碑或操作日志，只保留当前快照。

所有权威文件都采用同目录临时文件、磁盘刷新和 Windows 原子替换。便签提交同时校验版本和正文哈希；甘特图与 MFA 保存前校验启动时读取的文件指纹。检测到外部替换时不会静默覆盖，而是将待保存内容写入 `conflicts/`，由用户决定保留哪一版。甘特图与 MFA 保留最近 5 份写前备份。旧版本布局可用 [`scripts/migrate-petaldesk-storage.ps1`](scripts/migrate-petaldesk-storage.ps1) 迁移，完整取舍见 [本地存储设计](docs/storage.md)。

MFA 同样可以随整个数据目录迁移。首次使用时需要设置恢复密码；保险库正文由随机密钥进行 XChaCha20-Poly1305 认证加密，同一密钥分别由当前 Windows 用户的 DPAPI 和基于 Argon2id 的恢复密码包装。本机日常打开不需要输入密码；复制目录到新电脑或另一个 Windows 用户后，输入一次恢复密码即可解锁，并为新环境建立新的 DPAPI 包装。恢复密码不会保存，保险库、备份和冲突副本也不会降级为明文；忘记恢复密码且原电脑已不可用时，只能使用各服务提供的账户恢复码。

### 升级与兼容

`0.4.0` 可以直接覆盖安装。首次启动时会自动识别旧的 `飞花/.feihua` 存储布局、旧便签元数据和旧甘特图数组格式，转换到当前 `.petaldesk/` 结构；便签正文 `note.md` 不会被改写，甘特图转换前会保留迁移备份。普通版本升级不需要导出、导入或输入 MFA 恢复密码；恢复密码只在把 MFA 数据目录复制到另一台电脑或另一个 Windows 用户时使用。旧数据损坏或格式版本过新时，飞花会保留原文件并阻止静默覆盖。

## 安装与运行

下载 [Windows x64 安装包](https://github.com/starsliao/PetalDesk/releases/download/v0.4.0/PetalDesk_0.4.0_x64-setup.exe) 后按向导操作。安装器会检查 WebView2；缺少时从微软官方地址显示进度并下载、静默安装，然后继续安装飞花 - PetalDesk。联网安装包因此更小。

没有 WebView2 或下载权限时，安装器会明确提示失败原因，不会静默留下无法启动的程序。未签名构建可能显示“未知发布者”，这是 Windows 对代码签名的正常提示。

安装包已经包含长截图所需的 Native Messaging Host。浏览器增强模式还需要安装对应扩展；Firefox 面向普通用户的扩展必须经过 AMO 签名，具体发布方式见 [`docs/publishing.md`](docs/publishing.md)。不安装扩展仍可使用通用长截图。

## 技术栈

- Rust stable + Tauri 2：文件、窗口、托盘、截图、剪贴板、通知和本地 IPC。
- Svelte 5 + TypeScript：主界面、便签和小工具。
- CodeMirror 6：Markdown/纯文本编辑、中文输入法、撤回与重做。
- Chrome、Edge、Firefox 扩展：为浏览器长页面提供稳定的滚动控制和页面状态协作。
- 系统 WebView2：Windows 桌面渲染，避免 Electron 自带 Chromium 的体积。

## 开发

环境：Rust stable、Node.js、pnpm 和 Windows WebView2 Runtime。

```powershell
pnpm install
pnpm tauri dev
```

只看浏览器演示界面：

```powershell
pnpm dev
```

浏览器模式不会读写桌面版数据目录；便签等演示数据可以使用独立的 `localStorage`，MFA 验证器只使用内存模拟数据，也不会保存输入的密钥。

常用检查：

```powershell
pnpm check
pnpm test
pnpm build
cargo fmt --all --manifest-path src-tauri/Cargo.toml -- --check
cargo test --manifest-path src-tauri/Cargo.toml --offline
```

生成 Windows x64 联网安装包：

```powershell
pnpm package:windows
```

更完整的架构、发布和目录说明见 [`docs/README.md`](docs/README.md)。
