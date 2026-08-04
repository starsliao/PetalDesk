# 网站与版本发布

## 产品网站

网站源码位于 `website/`，其中 `index.html` 和 `assets/` 可以作为一个完整静态站点直接打开。推送影响 `website/**` 的 `main` 分支提交后，`.github/workflows/pages.yml` 会自动部署到：

<https://starsliao.github.io/PetalDesk/>

## 0.6.2 平台范围

Windows 10/11 x64 与 macOS 12+ 共享便签、甘特图、计时器、提醒、系统通知、MFA 和普通截图能力。`0.6.0` 在 Windows 新增本地密码保险库，并把经确认的自动填充和登录信息检测整合进 Firefox 长截图扩展；`0.6.1` 修复密码管理器首次启用时对 MFA 全局恢复密码的复用与修改体验；`0.6.2` 修复密码管理器提示层、窗口关闭和锁定交互，取消闲置自动锁定，并在远程桌面会话中阻止打开会被系统隐藏的 MFA 与密码管理器窗口。Chrome/Edge 的密码能力延期。Windows 继续提供长截图、自动滚动和 Native Messaging Host。macOS 使用 Keychain 提供 MFA 本机免密解锁，并支持普通区域截图，但当前不提供密码管理器、长截图、浏览器联动、Native Messaging 注册或自动更新。

macOS Release 是一个 Universal DMG，内部同时包含 `x86_64-apple-darwin` 与 `aarch64-apple-darwin`，所以 Intel 和 Apple Silicon 不需要两个安装包。当前发布资产为：

- `PetalDesk_0.6.2_x64-setup.exe`
- `PetalDesk_0.6.2_x64-setup.exe.sig`
- `latest.json`
- `PetalDesk_0.6.2_universal.dmg`
- `PetalDesk_Firefox_AMO-upload_0.6.2.zip`
- `PetalDesk_Firefox_AMO-source_0.6.2.zip`

Windows `0.5.2` 及后续版本可以通过客户端自动更新到 `0.6.2`，无需先在本机手工打包或安装。macOS 的 `0.5.0` 是首个公开版本，`0.6.2` 可以直接覆盖升级。MFA 保险库继续兼容已有 DPAPI/Keychain 包装；Windows 密码保险库使用独立数据密钥，并与 MFA 协调同一个全局恢复密码。

## 本地构建

在 Windows 上生成 x64 联网安装包：

```powershell
pnpm package:windows
```

安装包输出到 `src-tauri/target/release/bundle/nsis/`。

在 macOS 上生成 Universal 应用和 DMG：

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
pnpm install --frozen-lockfile
pnpm package:macos
```

DMG 输出到 `src-tauri/target/universal-apple-darwin/release/bundle/dmg/`。Universal 目标必须在 macOS 上构建；Windows 主机不能直接产出可发布的 DMG。

## 没有 Mac 时使用 GitHub Actions

推送与项目版本一致的 `v*` 标签后，[`.github/workflows/release.yml`](../.github/workflows/release.yml) 会完成以下工作：

1. 在 Ubuntu Runner 校验 `package.json`、Cargo、Tauri 和浏览器扩展版本是否与标签一致。
2. 在 Windows Runner 构建现有 NSIS 安装包和 Firefox 发布资产。
3. 在 GitHub 托管的 macOS 15 Runner 安装 Intel、Apple Silicon 两个 Rust target，并通过 `universal-apple-darwin` 生成一个 Universal DMG。
4. 两个平台构建成功后，由独立发布任务一次性创建或更新同一个 GitHub Release，避免 Windows 与 macOS 任务竞争创建 Release。

因此没有实体 Mac 也可以完成 macOS 打包。GitHub Runner 能验证编译、打包、签名和公证流程，但不能代替真实设备上的屏幕录制权限、快捷键、多显示器、Dock / 托盘和 Gatekeeper 安装体验测试；正式发布前仍应在 Intel 或 Apple Silicon 真机上至少完成一次验收。

## macOS 签名与公证

正式对外分发建议在仓库 Actions secrets 中同时配置以下三项签名信息：

- `APPLE_CERTIFICATE`：Base64 编码的 Developer ID Application `.p12` 证书。
- `APPLE_CERTIFICATE_PASSWORD`：导出 `.p12` 时设置的密码。
- `APPLE_SIGNING_IDENTITY`：证书中的 Developer ID Application 身份。

需要 Apple 公证时，再同时配置：

- `APPLE_ID`：Apple Developer 账号。
- `APPLE_PASSWORD`：该账号的 app-specific password。
- `APPLE_TEAM_ID`：Apple Developer Team ID。

正式标签工作流只接受两种状态：六项签名与公证 secret 全部配置，或六项全部留空。这样不会误发布“已签名但未公证”的中间状态。不要把证书、密码或 Apple 账号写入仓库文件或构建日志。

没有配置 Apple secrets 时，工作流仍会生成未签名、未公证的 Universal DMG，便于内部验证。但 Gatekeeper 可能阻止普通用户直接打开，用户会看到无法验证开发者或应用已损坏之类的提示。未签名构建不应作为无提示安装的正式发行质量承诺，也不应建议用户长期关闭系统安全检查。

macOS 普通截图首次运行依赖“系统设置 > 隐私与安全性 > 屏幕录制”授权。签名身份或应用标识变化可能导致系统把它视为新的授权主体，发布验收时必须重新确认权限行为。

## Windows 浏览器增强发布

Windows 包始终包含并注册 Firefox Native Messaging Host。构建 Chromium 增强时设置 `PETALDESK_CHROME_EXTENSION_ID` 和/或 `PETALDESK_EDGE_EXTENSION_ID`；GitHub 标签工作流从同名 repository secrets 读取 32 位商店扩展 ID。缺少某个 ID 只跳过对应 Chromium 浏览器的注册。

安装器不会安装浏览器扩展本身。公开 Firefox 版本还应发布 AMO 签名的 `browser-extension/dist/firefox` 构建；稳定 Gecko ID 已与 Host allowlist 对齐。Chrome 与 Edge 共用 Chromium 扩展构建，Firefox 使用独立 manifest 与签名 XPI。以上流程只适用于 Windows，macOS DMG 不包含或注册 Native Messaging Host。

公开页面使用以下稳定地址，客户端和审核材料不写死尚未确认的 AMO slug：

- Firefox 扩展安装页：<https://starsliao.github.io/PetalDesk/firefox.html>
- 浏览器增强隐私政策：<https://starsliao.github.io/PetalDesk/privacy.html>

AMO 公开提交前应先把包含这两个页面的 `main` 分支推送并等待 Pages 部署成功，再创建 `v0.6.2` 标签和提交 AMO，避免审核材料引用尚未上线的页面。

本地生成带版本号的 AMO 上传包：

```powershell
npm --prefix browser-extension run package:firefox
```

标签工作流总会附加明确标识的未签名 AMO 上传 ZIP 和审核源码 ZIP，但不会自动提交 AMO。首版应等 GitHub Release、Windows 安装包、隐私页和安装页全部公开后，再由产品所有者在 AMO Developer Hub 手工上传，避免桌面发布失败时提前占用不可重复的扩展版本号。未签名 ZIP 仅用于 AMO 上传，不能作为可直接安装的 Firefox 扩展对外提供。后续若增加 API 自动提交，应放在桌面 Release 成功后的独立工作流中，并单独处理 AMO 已存在同版本的幂等情况。

当前版本页面：

<https://github.com/starsliao/PetalDesk/releases/tag/v0.6.2>
