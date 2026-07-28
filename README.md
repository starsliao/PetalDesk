# 飞花 - PetalDesk

[产品网站](https://starsliao.github.io/PetalDesk/) | [下载 Windows 安装包](https://github.com/starsliao/PetalDesk/releases/download/v0.2.1/PetalDesk_0.2.1_x64-setup.exe)

飞花 - PetalDesk `0.2.1` 是一款面向 Windows 10/11 的本地便签应用。它以简洁的独立便签窗口为核心，同时提供 Markdown、纯文本、图片、搜索、回收站和一组可以随时唤起的小工具。正文与配置保存在用户指定的“飞花 - PetalDesk 数据存储”目录中，不依赖账号或云服务。

## 主界面

主界面用于查找和管理全部便签：

- 支持正文搜索；完整列表中的便签可以拖动手柄手动排列，顺序会随数据一起保存。
- 便签卡片展示排版后的内容摘要，不直接显示 Markdown 源码。
- 顶部只保留“设置”“回收站”和蓝色“小工具”入口。
- “新建便签”是便签列表最后一张较大的操作卡片；新便签会随机使用一种背景色，并追加到现有便签末尾。
- 设置对话框统一管理新便签的默认编辑样式、飞花 - PetalDesk 数据存储路径和全局截图快捷键。
- 修改数据存储路径后需要重启，界面会提供“立即重启”和“稍后”选项。

启动飞花 - PetalDesk 或再次运行程序时，会直接恢复最近使用的便签；没有便签时才显示主界面。双击系统托盘图标会打开手动顺序中的第一张便签，托盘右键选择“显示飞花 - PetalDesk”可以打开主界面，“便签列表”子菜单按相同顺序列出并打开指定便签。

## 便签

每张便签拥有独立窗口，可移动、缩放、置顶或进入只读模式。关闭窗口不会退出飞花 - PetalDesk，应用会继续驻留系统托盘；需要通过托盘菜单显式退出。

- 标题独立保存在 `meta.json`，点击标题即可编辑，默认以粗体显示。
- 删除便签前会二次确认，删除后进入回收站，可恢复或永久清空。
- 背景色包括黄色、粉色、蓝色、绿色、紫色、灰色，以及使用白色文字的炭笔色。
- 只读模式会隐藏编辑工具栏，并禁止修改标题和正文。
- 每张便签可以单独选择 `Markdown` 或 `纯文本`；创建后不会跟随默认样式改变。
- 点击置顶会把便签移到列表第一位；取消置顶不会再次改变它的位置。
- 便签标题栏提供新建、编辑样式、只读、颜色、置顶、删除、小工具和关闭等操作。

设置中的“默认编辑样式”只决定之后创建的便签。便签标题栏中的编辑样式按钮用于切换当前便签，按钮提示会显示当前样式。

### Markdown 编辑

Markdown 模式采用接近 Typora 的单栏即时排版：光标进入相应内容时显示必要的 Markdown 标记，离开后在原位置呈现排版效果；工具栏中的源码按钮可以临时切换完整源码视图。纯文本模式不会解析 Markdown。

当前支持：

- 标题、粗体、斜体、删除线和 `==高亮==`。
- 链接、裸 URL、行内代码、代码块和引用。
- 有序列表、无序列表、任务列表和分隔线。
- 图片粘贴、拖放和文件选择；导入的图片复制到当前便签的 `assets` 目录并使用相对路径。
- 撤销、重做、查找，以及常用 Markdown 格式化按钮。

Markdown 链接和直接输入的 URL 均可在按住 `Ctrl` 后单击，并交给系统默认浏览器打开。原始 HTML、表格、数学公式、脚注和第三方 Markdown 插件不属于当前版本范围。

## 小工具

小工具可以从主界面右上角、任意便签标题栏或系统托盘菜单打开。菜单中的截图项会同步显示当前快捷键，例如 `截图(F1)`。

### 计时器

- 无背景电子管数字显示，冒号在计时期间每秒闪烁，暂停后停止闪烁。
- 鼠标移入后显示控制层，可重置、暂停/继续、展开记录或关闭。
- 记录包含操作时间、动作和当时的计时时间，支持按动作筛选及确认后清空。
- 可拖动和缩放；窗口位置、尺寸与数字透明度会保存，下次打开继续使用。
- 每次打开从 `00:00` 开始并记录一次重置，关闭时记录一次暂停。

### 提醒

- 支持指定时间执行一次，以及按固定间隔重复。
- 支持每天、每周、每月和每年周期。
- 到点发送 Windows 通知；飞花 - PetalDesk 需要保持运行或驻留系统托盘。
- 月末和闰年日期会调整到对应月份中的有效日期。

### 任务甘特图

- 三列布局：任务名称、进度和时间轴。
- 支持新建任务、右键编辑或二次确认后删除，以及拖动调整任务顺序。
- 进度分为未开始、进行中和已完成，可直接筛选；任务名称和时间条会随状态显示不同样式。
- 支持拖动任务时间条或两端调整时间范围，也可拖动时间轴空白区域水平浏览。
- 时间轴支持按钮与鼠标滚轮缩放，粒度可细化到小时；提供左右移动、重置和定位到首个任务等控制。

### 截图

截图工具默认使用全局快捷键 `F1`，可以在设置中重新录入。新快捷键发生冲突时不会替换已经可用的快捷键。

第一阶段截取鼠标所在的单个显示器，使用手动框选，选区不会跨显示器；暂不提供窗口或控件边界自动识别。完成选区后可以移动选区、通过八个方向的控制点调整尺寸，并查看像素尺寸。选区内双击与点击“复制”效果相同。

标注工具包括：

- 矩形、椭圆、直线、单箭头和双箭头。
- 铅笔、马克笔、文字。
- 马赛克、高斯模糊和橡皮擦。
- 颜色、线宽、填充、实线/虚线、笔头、字体、字号、粗体、斜体、下划线、范围和强度等参数。
- 撤销、重做、取消、置顶贴图、另存为 PNG 和复制到剪贴板。

鼠标移动时的放大镜会显示坐标和颜色值；按 `C` 复制颜色，按 `Shift` 切换颜色格式。置顶会生成独立的无边框贴图窗口，贴图支持拖动、等比缩放，以及右键复制、另存为和关闭。贴图仅保留在当前飞花 - PetalDesk 进程中，退出后不会恢复。

常用截图快捷键：

| 操作 | 快捷键 |
| --- | --- |
| 启动截图 | `F1`（可修改） |
| 取消 | `Esc` |
| 撤销 | `Ctrl+Z` |
| 重做 | `Ctrl+Y` / `Ctrl+Shift+Z` |
| 复制截图 | `Ctrl+C` |
| 保存截图 | `Ctrl+S` |
| 复制取色值 | `C` |
| 切换取色格式 | `Shift` |

复制或保存成功后会结束当前截图；取消保存或处理失败时保留截图界面，以便继续操作。

## 数据存储

默认“飞花 - PetalDesk 数据存储”位于用户“文档”目录下的 `PetalDesk` 文件夹。安装时可以选择其他目录，安装后也可以从主界面的“设置”中更改。

```text
PetalDesk/
  .petaldesk/
    notes/<note-id>/
      note.md
      meta.json
      assets/
    config.json
    state/
      windows.json
      note-order.json
    tools/
      timer.json
      reminders.json
      gantt.json
      screenshot.json
    backups/
    journal/
    trash/
    conflicts/
```

`note.md` 是便签正文的唯一真相，保持标准 Markdown；标题、颜色、置顶、只读和编辑样式等元数据保存在相邻的 `meta.json`。主界面的手动顺序保存在 `.petaldesk/state/note-order.json`。计时器、提醒、甘特图与截图设置分别保存在 `.petaldesk/tools/` 下，其中 `screenshot.json` 包含截图快捷键、上次保存目录、取色格式和工具参数。

`%LOCALAPPDATA%\PetalDesk\` 只保留当前数据存储路径指针、可重建的搜索索引和调试日志，这些内容不属于需要迁移的业务数据。路径指针位于 `%LOCALAPPDATA%\PetalDesk\storage-path.txt`，使用带 BOM 的 UTF-16LE 编码以支持完整的 Unicode 路径。

## 数据迁移

迁移到新电脑时，先从托盘显式退出飞花 - PetalDesk，再复制整个“飞花 - PetalDesk 数据存储”目录。安装飞花 - PetalDesk 时选择复制后的目录，或安装完成后在设置中更改目录并按提示重启；搜索索引会自动重建。

仓库同时提供旧版目录迁移脚本。默认命令将“文档\PetalDesk”中的旧布局原地升级：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\migrate-petaldesk-storage.ps1
```

从自定义旧目录迁移到另一个目录：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\migrate-petaldesk-storage.ps1 `
  -SourceRoot "D:\旧版 PetalDesk" `
  -TargetRoot "E:\PetalDesk 数据"
```

脚本逐文件复制并校验 SHA256，不会覆盖内容不同的目标文件。原地升级时，旧根目录下的 `notes` 会在校验成功后归档到 `.petaldesk\backups\migration-时间戳\legacy-notes`；跨目录迁移会完整保留源目录。每次执行都会在目标目录的迁移备份中生成 JSON 报告，并更新当前数据存储路径指针。

## Windows 安装包

生成 Windows x64 联网安装包：

```powershell
pnpm package:windows
```

安装向导允许选择“飞花 - PetalDesk 数据存储”路径，默认是用户“文档”目录下的 `PetalDesk` 文件夹；目标文件夹尚不存在时会自动创建并检查写入权限。

安装器会先检查 WebView2 Runtime。系统缺少运行库时，从微软官方地址下载并显示下载百分比、速度和剩余时间，然后静默安装 WebView2；成功后才继续安装飞花 - PetalDesk。下载或安装失败会中止安装，避免留下无法启动的应用。联网安装包不内置 WebView2，因此体积较小，并要求安装期间可以访问微软下载服务。

自行构建的安装包如果没有配置 Windows 代码签名证书，Windows 或第三方安全软件可能显示“未知发布者”或行为提醒；正式分发应为安装器和应用可执行文件配置可信代码签名。

## 技术栈

- Rust stable + Tauri 2：本地文件、窗口、托盘、截图、剪贴板、通知和 IPC。
- Svelte 5 + TypeScript：主界面、便签与小工具界面。
- CodeMirror 6：Markdown/纯文本编辑、中文输入和撤销重做。
- 系统 WebView2：Windows 桌面界面运行时。

## 本地开发

环境需要 Rust stable、Node.js、pnpm，以及可用的 WebView2 Runtime。

```powershell
pnpm install
pnpm tauri dev
```

只运行浏览器端界面：

```powershell
pnpm dev
```

浏览器模式使用独立的 `localStorage` 演示数据，不会读写桌面版的飞花 - PetalDesk 数据存储；截图、系统托盘、原生通知等 Windows/Tauri 能力需要在桌面模式验证。

## 开发验证

```powershell
pnpm check
pnpm test
pnpm build
cargo fmt --all --manifest-path src-tauri/Cargo.toml -- --check
cargo test --manifest-path src-tauri/Cargo.toml --offline
```

构建不带安装器的桌面 Release 程序：

```powershell
pnpm tauri build --no-bundle
```
