# 密码填充与保存功能交接文档（2026-08-07）

> 读者：接手修复的开发者（codex）。
> 范围：飞花密码管理器的**浏览器填充**（fill）与**登录保存提示**（capture）两条功能链。
> 前置文档：`docs/PASSWORD_FIREFOX_HANDOFF.md`、`docs/PASSWORD_FIREFOX_V0.7.0.md`、`docs/PASSWORD_CHANNEL_DEBUG_HANDOFF.md`（通道死锁修复全过程）。

## 0. 当前状态速览

| 链路 | 状态 | 证据 |
| --- | --- | --- |
| 通道（桌面↔host↔扩展） | ✅ 已修复并验证 | 连接稳定数小时；探针实测 ping/getStatus 50ms 应答 |
| `openPasswordManager`、`setCaptureEnabled`、状态同步 | ✅ 真机验证 | 弹窗"打开飞花密码管理器"可用，检测开关显示已开启 |
| 角标（badge） | ✅ 真机验证（切换标签与刷新均正常） | 2026-08-07 用户实测确认；修复见 §4.3 |
| 弹窗点击填充 | ❌ 真机不工作（至少对 163 邮箱） | 见 §4.1 |
| 登录提交→保存提示 | ❌ 真机从未观察到提示 | 见 §4.2 |

**重要**：同站 iframe 支持（163 修复的关键）已实现并全部单测通过，代码在 commit `fc933e4`（main 分支，未发版）。2026-08-07 用户实测：角标在切换标签与刷新后均正常——证明新扩展 + 新桌面端已在本机运行且 badge 链路（含 `fc933e4` 的角标重放修复）真机有效；但填充与保存提示仍不工作。接手后按 §4.1/§4.2 的嫌疑顺序用 §5 工具实测定位。

## 1. 功能需求（用户明确要求，已逐条确认）

1. 打开网站时，若保险库有该站点账户，扩展角标显示数量（1/2/3…）。
2. 无记录时用户手动登录，**提交登录按钮后立即提示保存**（不等成功信号——登录失败也提示，用户点忽略）。
3. 有记录但输入的用户名或密码不一致时，提示"保存为新账户"或"更新既有账户"（多账户可选更新哪个）；完全一致不提示。
4. 弹窗点账户**直接填充**（不要页面二次确认），填充后页面 2 秒轻提示，绝不自动提交。
5. 弹窗账户行右侧菜单：复制账号 / 复制密码（桌面端写剪贴板，凭据不经扩展）/ 删除（二次确认）。
6. 登录检测**强制开启**，密码管理器里不再有开关。

## 2. 链路与关键文件

```
弹窗/页面 ──runtime message──> background (password-bridge.js) ──native port──> host (petaldesk-browser-host.exe)
                                                                                    │ named pipe (overlapped I/O)
桌面 PasswordBrowserService (password_browser.rs) <── BrowserSecretBridge (browser_secret_bridge.rs) ┘
```

- 扩展：`browser-extension/src/background/password-bridge.js`（协议核心）、`src/content/password-manager.js`（页面内字段识别/填充/提交检测/浮层）、`src/popup/popup.{js,html,css}`（弹窗）、`src/shared/password-templates.js`（模板+`exactOrigin`+`sameSite`）。
- 桌面：`src-tauri/src/password_browser.rs`（事件编排）、`browser_secret_bridge.rs`（管道服务）、`browser_native_host.rs`（host 进程）、`passwords.rs`（保险库、`capture_decision_at` 匹配规则）。
- 协议清单：`browser-extension/README.md` 最准（随代码更新）。

## 3. 已修复（有证据，勿再怀疑）

1. **双端同步管道死锁**（0.6.3 以来"请求必超时"的根因）：Windows 同步管道句柄挂起阻塞读时，另一方向的写永久阻塞。两端已改 `FILE_FLAG_OVERLAPPED` 重叠 I/O（commit `b6595d3`，含"挂起读时写必须 2s 内完成"的真实管道回归测试）。详见 `PASSWORD_CHANNEL_DEBUG_HANDOFF.md`。
2. **扩展上报过期 origin**：标签页激活改用实时 URL（不再用加载时缓存）；非网页标签通知桌面清跟踪。

## 4. 现存问题与嫌疑点

### 4.1 弹窗点击填充无反应（用户实测，0.7.2 扩展 + 修复版桌面）
用户点弹窗账户后显示提示但页面无浮层/无填充。已实现的修复（`fc933e4`，未验证）：fillOffer 广播到全 frame，含登录字段的同站 iframe 自动确认并绑定，`fillSecret` 定向该 frame。若用新扩展实测仍失败，按此顺序查：

1. **扩展没换/页面没刷新**：重载扩展 zip 后必须**刷新目标页面**——content script 只在页面加载时注入，老标签页没有它，fillOffer 永远无人接收。
2. **广播响应语义**：`tabs.sendMessage` 不带 frameId 时，Promise 只拿到**最快应答的那个 frame** 的返回值。background 对广播 fillOffer 的返回值处理要复查：某个 frame 快速返回 `{ignored:true}` 时不能误伤真正确认的 frame（`dispatchFillOffer` 的返回值/session 状态推进在 password-bridge.js）。
3. **iframe 动态创建**：163 的登录 iframe 是页面加载后动态插入的；content script 注入时机（`document_idle`）可能早于 iframe 创建，iframe 里的脚本实际何时注入、是否收到广播，要在真实页面验证（playwright 可加载真实 163 页面 + 手动 `addScriptTag` 注入验证，参照 `test-results/inspect-163-content.mjs`）。
4. **字段识别失败**：iframe 里的表单结构若不被 `identifyLoginFields` 识别（歧义即失败关闭），填充静默放弃。用 §5 的注入脚本直接在该 frame 里调 `identifyLoginFields` 看返回。
5. 桌面端诊断点会给出答案：密码管理器→连接诊断→复制，看 `fill offer-fill-direct ok=…` 还是 `fill-request-rejected reason=…`。

### 4.2 登录提交不提示保存（用户从未见过提示）
已实现（0.7.3）：提交即发 `capture-submitted` + `capture-success`（无成功判定）。同站 iframe 捕获在 `fc933e4`。排查顺序：

1. 同上：扩展版本 + 页面刷新 + iframe 注入时机。
2. **`location.ancestorOrigins` 回退风险**：iframe 内 candidate 的 `origin` 取顶层 origin（`ancestorOrigins` 末位），取不到则回退为 iframe 自身 origin（`dl.reg.163.com`）。回退后与条目 origin（`mail.163.com`）不匹配 → 桌面端 `capture_decision_at` 找不到同 origin 账户 → 按"新增"提示或 origin 校验失败静默丢弃。Firefox 140+ 支持 `ancestorOrigins`，但要在真实页面确认返回顺序与值。
3. 提交检测选择器：`scheduleCandidate` 靠 form submit + 类提交按钮 click。163 的登录按钮是 JS 异步登录（不真正 submit form），`onClick` 分支的选择器是否覆盖该按钮需要真实页面验证。
4. 桌面端 `handle_capture_candidate`（password_browser.rs:1162 附近）：`frameOrigin`/`origin` same-site 校验、promptOrigin 校验，任何一步不过都静默 return——诊断点目前只覆盖 badge/fill，**建议给 capture 候选加同样的 record_event 记录点**（为什么丢弃）。

### 4.3 角标（已修复并真机确认）
~~刷新后不恢复~~。修复内容（`fc933e4`）：tab-ready 时用 tabAccounts 缓存立即重放角标 + 激活时读实时 URL + 非网页标签通知桌面清跟踪。2026-08-07 用户实测：切换标签与刷新均正常显示。

### 4.4 其他已知限制（桌面端 agent 标注）
- iframe 绑定的填充会话遇顶层导航会被 `bind_fill_tab_ready`（仍要求 frameId==0）清掉——两步登录第二页可能受影响。
- `resume_pending_fills` 只捞 frameId==0 的会话。

## 5. 调试工具箱（全部在 `test-results/`，git 不跟踪）

| 工具 | 用途 |
| --- | --- |
| `repro-secret-channel.mjs <host.exe>` | 假桌面+真 host+假扩展，验证 host 全链路（成功标志 `FULL ROUNDTRIP OK`） |
| `probe-live-host.mjs` | 劫持 endpoint，假桌面驱动**真 host+真扩展**，可发任意 `password.*` 命令、看扩展事件 |
| `probe-real-desktop.mjs` | 探测真桌面管道服务器是否存活（会被 exe 校验拒绝，属预期） |
| `inspect-163.mjs` / `inspect-163-content.mjs` | playwright 加载真实 163 页面，检查 frame 结构/字段/注入 content script 后的行为 |
| host 日志 | `%LOCALAPPDATA%\PetalDesk\browser-bridge\host-diagnostics.log` |
| 桌面诊断 | 密码管理器 → 连接诊断 → 复制诊断信息（含 `badge`/`fill` 业务事件） |

**强烈建议**：用 playwright 持久化上下文 + `--load-extension` 把真实扩展装进真实 Chromium（native messaging 需把 manifest 注册到 `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.petaldesk.capture`，模板在 `browser-extension/native-host/manifests/chromium.json.template`），可以无人值守地复现整条链路，比手工点可靠得多。

## 6. 本机环境状态

- 桌面端 exe：`%LOCALAPPDATA%\PetalDesk\petaldesk.exe` 是本地构建的 0.7.3+iframe 修复版（备份：`petaldesk.prev3` 等）；host exe 同理为修复版（备份 `.old-0.7.1`）。
- 扩展：`browser-extension/dist/firefox`（当前源码构建，含 iframe 支持）；zip 产物在 `browser-extension/dist/artifacts/`。用户通过 about:debugging 临时加载 zip——**重载 zip 不会更新内容，必须移除后重新载入**。
- 版本：0.7.3 已正式发布（不含 iframe 支持）；iframe 支持在 main（`fc933e4`）未发版，验证通过后建议发 0.7.4（版本号 7 处同步，release.yml 会校验；GitHub 故障时注意 tag 事件可能被吞需重推）。
- 部署手法：进程占用 exe 时先 `mv` 改名再 `cp`；重启桌面端生效；扩展重连会自动拉起新 host。

## 7. 测试基线（接手时应该全绿）

- `cargo test --manifest-path src-tauri/Cargo.toml`：361 passed / 0 failed / 1 ignored（剪贴板用例在远程桌面会话可能环境性失败，属已知）。
- `pnpm test`：337；`pnpm check`：0/0。
- `cd browser-extension && npm test`：81；`npm run build` 通过。

## 8. 安全边界（修复不许破坏）

密码只走内存管道与 fillSecret transient 路径；不写文件/扩展存储/日志；填充绝不自动提交；跨站（非 same-site）iframe 一律拒绝填充与捕获；HTTP 逐 origin 显式允许；诊断只记命令名/ID/原因/origin，不含用户名密码 token。
