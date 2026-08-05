# 飞花密码管理器 Firefox 扩展 v0.7.0 改动说明

> 版本：`0.7.0`
> 前置文档：`docs/PASSWORD_FIREFOX_HANDOFF.md`（v0.6.3 故障交接）、`docs/PASSWD_PLAN.md`（原始计划）
> 本文档记录本轮修复与新增功能的完整改动，供后续维护与 AMO 审核参考。

## 1. 本轮目标

1. 浏览网站时，扩展角标显示该站点在飞花保险库中的账户数量（1/2/3…）。
2. 站点无记录时，用户登录成功后页面浮层提示"保存到飞花"。
3. 站点有记录但输入的用户名或密码不一致时，提示"保存为新账户"或"更新既有账户"（多账户可选择更新哪个）；完全一致不提示。
4. 修复 v0.6.3 遗留的通信可靠性、假健康与性能问题，删除 Firefox 二次授权流程，使整个功能真实可用。

## 2. 根因与修复对照

| 根因 | 现象 | 修复 |
| --- | --- | --- |
| 保险库会话绑定密码窗口，窗口关闭即停用捕获/填充 | 正常浏览时功能全部不可用 | 新增"浏览器后台会话"（见 §3.1） |
| `authenticationInfo` 为可选权限，需工具栏二次授权 | 流程繁琐、状态机复杂 | 改为安装时必选，删除全部 consent 流程（见 §3.2） |
| 单次 `password.getStatus` 超时即退休整个连接 generation | 超时→重连→同步→再超时抖动循环 | 超时后先 ping 探测，连接健康则保留（见 §3.4） |
| Native Host secret connector 永久退出后主循环仍刷心跳 | "心跳正常但通道已死"假健康 | connector 不可恢复故障 → Host 进程退出，Firefox 自动重拉（见 §3.4） |
| Host 出站队列 Full 时静默丢 `secret.response` | 桌面端只能等超时 | 记录结构化诊断并明确重建连接（见 §3.4） |
| `connectionReady` 同步含 `requestConsent` 往返、填充前有 `getStatus` 预检、状态轮询触发重复捕获广播 | 卡顿与通道争用 | 删除多余往返；捕获同步幂等（见 §3.3） |
| 各层错误被压成统一 `unknown` | UI 只剩"通信异常"，无法定位 | 分层状态 + 诊断环形缓冲 + 前端错误字段保留（见 §3.5） |
| "用户名不同"只提供新增，不能更新既有账户 | 多账户场景无法更新 | `Create` 决策附带同 origin 账户列表，支持选择替换（见 §3.3） |

## 3. 详细改动

### 3.1 保险库浏览器后台会话（`src-tauri/src/passwords.rs`）

- `SessionState` 新增 `browser_active: AtomicBool`。
- 新 API：`activate_browser_session()`（幂等）、`require_any_epoch()`（窗口激活或浏览器会话激活均可）、`browser_fill_data_at(entry_id, epoch)`。
- `validate_epoch` 放宽接受浏览器会话；`deactivate()` 仍 bump epoch，窗口关闭前排队的窗口工作依旧失效。
- 浏览器会话激活时，窗口关闭保留解密 vault（仍清剪贴板）；`lock()` / `lock_current_session()`（手动锁定）立即终止浏览器会话并清除解密状态。
- 解锁方式不变：Windows DPAPI 绑定当前用户，无需用户交互；手动锁定后后台访问立即停止。

### 3.2 权限必选化与 consent 删除

- `browser-extension/manifest/firefox.json`：`data_collection_permissions.required = ["websiteActivity", "authenticationInfo"]`，删除 `optional`。
- 扩展删除：`CONSENT_ARM_TTL_MS`、`armAuthenticationConsent()`、`requireAuthenticationConsent()`、`handleAuthenticationConsentLoss()`、toolbar `onClicked` 授权处理器、`permissions.onRemoved` 监听、`consentRequired`/`consentChanged` 事件。
- `password.requestConsent` 保留为 no-op（恒返回 granted），兼容旧桌面端。
- 桌面端 `sync_capture_from_store()` 不再先发 `password.requestConsent`，直接按飞花检测开关下发 `password.setCaptureEnabled`。
- 飞花 UI 删除"等待 Firefox 授权"横幅、"授权后填充"按钮文案及全部 `action-required` 状态；保留"开启检测/暂不开启"产品级开关（仅控制检测逻辑，与权限无关）。

### 3.3 捕获/保存语义补全

- `capture_decision_at` 的 `Create` 分支附带同 origin 账户列表（≤16 条 `PasswordCaptureAccount`），使"用户名不同"时可选择更新既有账户。
- `handle_save_decision` 新增合法组合 `("new", "replace")`（entryId 必须在候选账户列表内）；replace 且候选用户名非空时用候选用户名+密码整体替换目标账户（保留站点名/登录地址/备注/模板/HTTP 允许标记），候选用户名为空维持保留原用户名；用户名冲突经 `ensure_unique_account` 返回错误回执。
- 保险库手动锁定时，`handle_capture_candidate` 发送 action `"locked"`，页面浮层提示先解锁；其余决策错误维持 `"same"`（不打扰用户）。
- 扩展 `captureMatch` action 集合：`new`（可带 accounts）/ `update` / `same` / `select` / `username-required` / `locked`；`onSaveDecision` 的 `replace` 在 `select` 与 `new` 下均合法。
- 内容脚本浮层：`new` + 多账户时显示"保存为新账户"+ 每账户"更新 \<username\>" + "忽略"；新增锁定提示浮层。

### 3.4 通信可靠性

桌面端 `browser_secret_bridge.rs`：

- 请求超时后先以 1.5s 发 `ping`（`validate_command` 放行该精确命令；由扩展 native-bridge 直接应答，不经 password-bridge）：ping 活 → 只向调用方返回超时错误，保留 generation；ping 死 → 退休 generation 重建。`try_send` Full/Disconnected、reader/writer 失败仍立即退休。
- 新增诊断环形缓冲（容量 100，命令名 + requestId 前 8 位 + 原因，**绝不含 token/用户名/密码**），供状态接口输出。

Native Host `browser_native_host.rs`：

- connector 不可恢复故障（握手线程卡死、worker 卡死）→ 置 fatal 标志 → 主循环返回 Err → 进程退出，Firefox 重连时自动拉起新 Host。
- 主循环向 secret outbound 队列 `try_send` 失败 → 写诊断日志（不含 payload）→ 请求 connector 丢弃当前连接并重建。
- 新增 `%LOCALAPPDATA%/PetalDesk/browser-bridge/host-diagnostics.log`：单行 JSON 诊断（启动时 >256KB 截断保留尾部），记录 connector 生命周期、握手结果、fatal 等，严禁记录秘密字段。

### 3.5 分层状态与前端

- `PasswordBrowserStatus` 删除 `consentArmed`/`consentActionRequired`；新增 `stdioConnected`（读 browser-bridge session 文件心跳，6s 内为活）、`pipeConnected`、`connectionId`、`diagnostics`（最近 20 条）、`lastRequestOutcome`；`extensionVersion` 现为 session 文件中的真实值；`capturePermission` 只产生 `granted`/`unknown`/`unavailable`。
- `src/lib/passwords.ts`：`command()` 抛出的错误保留 `code`/`layer`/`requestId`/`connectionId`；`getBrowserStatus()` 失败时保留 `errorCode` 与后端 detail，不再吞成统一 disconnected。
- `PasswordManagerTool.svelte`：通信异常横幅按分层字段给出具体文案（stdio 未连接 / 通道未建立 / 其他）；新增默认折叠的"连接诊断"区块（权限、stdio、密码通道、最近请求、扩展版本、连接 ID）+ "复制诊断信息"按钮。

### 3.6 角标与 popup（新协议）

新增事件（扩展→桌面）：

- `originActive { tabId, origin }`：tab-ready、tab 激活、`secretConnected` 重连后发送；空 origin 表示无有效页面。
- `fillRequest { entryId, tabId, origin, documentId }`：popup 点击账户发起当前页填充。
- `openPasswordManager {}`：popup 按钮唤起飞花密码窗口。

新增命令（桌面→扩展）：

- `password.updateBadge { tabId, origin, locked, accounts: [{entryId, username, siteName}] }`：扩展设置 per-tab 角标数字（0 或锁定时清除）并缓存账户列表供 popup 读取；accounts ≤16 条。
- `password.offerFillDirect { sessionId, entryId, offerId, tabId, frameId, documentId, origin, username, userTemplate, allowInsecureHttp }`：不经 `password.open` 直接建立绑定会话并下发填充浮层；扩展校验实时 tab URL 与 origin 匹配。

桌面端编排（`password_browser.rs`）：

- `badge_tabs` 映射（connection → tab → origin）；`originActive` → 计算并推送；条目 CRUD / 捕获保存 / 锁定 / 解锁后 `refresh_badges()` 全量重推；`connectionClosed` 清理映射。
- `fillRequest` → 校验 entryId/origin 精确匹配 → 创建预绑定 FillSession → `password.offerFillDirect`；后续 fillConfirm → `password.provideCredentials` 流程与既有完全一致（页面浮层二次确认后才下发密码）。
- `openPasswordManager` → 复用 `commands.rs` 提取的 `open_password_window()`（保留远程桌面守卫）。

popup（`browser-extension/src/popup/`，manifest `action.default_popup`）：

- 分层诊断区 + 当前站点账户列表 + "打开飞花密码管理器"；点击账户提示"请在页面中确认填充"。
- runtime 消息强制校验 `sender.id === runtime.id && !sender.tab`，拒绝 content script 伪造。
- 全部 DOM API + `textContent` 构建，无 innerHTML 拼接。

### 3.7 其他

- 版本号 0.6.3 → 0.7.0：`package.json`、`browser-extension/package.json`、`manifest/firefox.json`、`manifest/chromium.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock`、`src-tauri/tauri.conf.json`。
- AMO 文案（PERMISSIONS/PRIVACY/LISTING/REVIEWER_NOTES/SCREENSHOTS/SUBMISSION_CHECKLIST）与扩展 README 已同步。
- `docs/PASSWORD_FIREFOX_HANDOFF.md` 文末追加"§23 v0.7.0 已完成的修复"。

## 4. 安全边界（保持不变）

- 密码只走内存 named pipe；不写文件 spool、扩展 storage、日志、诊断。
- 填充必须经页面浮层二次确认（含 popup 发起）；只填字段，绝不自动提交。
- HTTP 逐 exact origin 显式允许；session/connection/tab/frame/document/origin/entry 绑定校验全部保留。
- 手动锁定立即停止后台访问并清理敏感内存；远程桌面敏感窗口限制不变。

## 5. 测试与验证

- Rust：`cargo test` 346 passed / 0 failed（新增：浏览器会话生命周期、Create 带账户列表、replace 整体替换、ping 探测两分支、诊断缓冲截断、fatal 传播、Host 日志、badge 推送、fillRequest、sync 幂等、分层 status）。
- 前端：`pnpm test` 335/335；`pnpm check` 0 errors 0 warnings。
- 扩展：`npm test` 57/57；`npm run build` 与 `package:firefox` 通过（AMO zip 含 popup）。
- 真实 Firefox E2E（需人工）：干净 profile 安装签名 XPI 并重启、角标计数、关闭密码窗口时的捕获提示、新增/更新选择、弹窗与飞花 UI 诊断一致、三组件各自重启后恢复、长截图回归。

## 6. 遗留事项

- 真实 Firefox 环境的全链路验证仍需人工执行（单元测试无法覆盖 `runtime.connectNative`、注册表、真实 pipe 往返）。
- 六个内置站点模板未做真实站点冒烟测试。
- AMO 上架需用 release 产物中的 `PetalDesk_Firefox_AMO-upload_0.7.0.zip` 重新提交审核。
