## 主要更新

- 新增 Google Chrome 浏览器扩展支持。Chrome 版与 Firefox 版使用同一套工具栏弹窗和密码能力，包括站点账户角标、连接诊断、登录检测开关、账户填充与复制、关联 TOTP、右键删除确认，以及主界面、密码、MFA 和便签四个快捷入口。
- 修复 HTTP 登录页不会提示保存密码的问题。登录检测现在覆盖所有 HTTP 与 HTTPS 站点，用户提交新的 HTTP 登录信息后也会看到保存或更新提示。
- HTTP 密码仍保持明确的安全边界：保存时显示明文传输风险，接受后只为该精确 origin 允许后续填充；TOTP 仍只用于受信任的 HTTPS 登录流程。扩展始终不会主动提交表单。

## Chrome 安装与连接

- Windows 安装器为 Chrome 和 Edge 注册同一个固定 Chromium 扩展 ID：`eclmlkbklcpcaijloflmohfpeldcbjkp`。构建和发布流程会从扩展清单公钥重新推导该 ID，防止 Native Messaging allowlist 与实际扩展身份不一致。
- GitHub Release 新增 `PetalDesk_Chrome_unpacked_0.8.5.zip`。该 ZIP 需要解压到固定目录，再从 `chrome://extensions` 开启开发者模式并选择“加载已解压的扩展程序”。
- 本次只发布 GitHub 开发者模式 ZIP，不声称已经在 Chrome Web Store 上架；安装器也不会静默安装浏览器扩展。

## 发布与兼容

- Release 固定包含 Windows 安装包及更新签名、macOS Universal DMG、Firefox AMO 上传包与审核源码包、Chrome 开发者模式 ZIP 和 `latest.json`，合计 7 个公开资产。
- Windows 自动更新的 `latest.json` 契约保持不变，仍只包含 `windows-x86_64`、安装包 URL 和签名；Chrome 扩展不会进入桌面自动更新清单。
- 本次没有改变密码或 MFA 保险库格式、恢复密码、存储路径或跨设备恢复规则。

## 验证

- 浏览器扩展测试覆盖 Firefox 与 Chrome 的共享密码桥、HTTP 保存提示、工具栏弹窗、Native Messaging 重连、填充、保存、TOTP 和安全拒绝路径。
- 发布打包只执行一次扩展测试与构建，同时生成 Firefox 两个归档和 Chrome ZIP；最终 Chrome ZIP 会再次校验版本、固定 ID、popup 和密码脚本完整性。
- GitHub Release 发布后会下载公开 Chrome ZIP 重复执行同一结构与身份校验，并确认 7 个资产均可公开访问。
