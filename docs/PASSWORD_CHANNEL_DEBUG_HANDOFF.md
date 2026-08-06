# 密码通道调试交接文档（2026-08-06）

> 读者：接手继续修复的开发者（codex）。
> 范围：飞花桌面端 ↔ Native Messaging Host ↔ Firefox 扩展的密码通道。
> 前置文档：`docs/PASSWORD_FIREFOX_HANDOFF.md`（0.6.3 时代故障）、`docs/PASSWORD_FIREFOX_V0.7.0.md`（0.7.0 功能改造）。

## 1. 一句话结论

**从 0.6.3 起"password.getStatus 必超时、连接每 4 秒重连一轮"的根因已定位并修复**：Windows 同步命名管道句柄在同一连接实例上"有挂起阻塞读时，另一方向的写会永久阻塞"。host（客户端）与桌面 bridge（服务器端）都用了同步句柄 + try_clone 分线程读写，两端都会死锁。两端已全部改为 `FILE_FLAG_OVERLAPPED` 重叠 I/O。修复后连接可稳定数小时。

剩余未闭环：站点角标（badge）与弹窗直接填充（fill）在真机上仍不稳定/不工作，见 §5。

## 2. 根因与证据

### 2.1 机理

- 同步管道句柄（无 `FILE_FLAG_OVERLAPPED`）上，一个线程 `ReadFile` 挂起等待数据时，另一线程在同一管道实例上的 `WriteFile` 会永久阻塞（即使管道缓冲远未满）。服务器端（`CreateNamedPipeW`、PIPE_WAIT）与客户端（`OpenOptions`/`CreateFile`）表现一致——均有探针测试证实。
- 握手能成功是因为握手是**串行**的（写 hello → 读 ready），当时还没有挂起的读。握手之后 reader 线程立刻挂起阻塞读，此后 writer 的第一笔写就死锁。
- 表现：桌面端每个 `password.*` 请求都 2s 超时 → ping 探测也常失败 → 退休当前 generation → 关闭管道 → host 重连 → 循环。session 心跳正常（主循环没死），形成"假健康"。

### 2.2 定位过程（可复用的方法论）

1. 桌面端诊断环（0.7.0 新增）显示 `request timeout` + `probe-failed`，证明命令无响应。
2. 用假桌面 + 真 host + 假扩展夹住中段（脚本见 §4），逐段验证：host→扩展 stdout 正常；**扩展应答进入 host stdin 后消失**——锁定 host 回程。
3. 给 host 加临时 `eprintln` 桩（事后已全部移除），确认卡在 writer 线程的 `write_all` 内部。
4. 修复 host 后，桌面端请求仍超时 → 用真实管道对写探针测试（`server-end write while read pending = false`），证实桌面端服务器句柄同病。

## 3. 已完成的修复（commit `b6595d3`）

只改了两个 Rust 文件，均含真实管道回归测试：

- `src-tauri/src/browser_native_host.rs`（host 进程）：
  - 客户端句柄 `FILE_FLAG_OVERLAPPED` 打开；握手/读/写全部重叠 I/O（`SecretPipeEvent`/`SecretPipeIo`/`secret_pipe_read*`/`secret_pipe_write*`，:720-1040 一带）。
  - 停止语义：`CancelSynchronousIo` → 每代 stop 事件 + `CancelIoEx`；握手全程 2s 上限（不再有永久卡死的握手线程）。
- `src-tauri/src/browser_secret_bridge.rs`（桌面端 bridge）：
  - `CreateNamedPipeW` 加 `FILE_FLAG_OVERLAPPED`，`ConnectNamedPipe` 改重叠；握手、reader/writer 全部重叠 I/O。
  - `SecretConnectionControl.retire()`：改为 stop 事件信号 + `CancelIoEx`，保留 `DisconnectNamedPipe`；所有 retire 调用点语义不变。
- 测试：cargo 全量 349 通过；核心回归"有挂起读时写必须在 2s 内完成"两端各一份。

**注意**：这两处修复随 0.7.2 发布。修复版的 host 与桌面端 exe 在发布前已手工部署到本机安装目录（见 §6）。

## 4. 调试工具箱（都在 `test-results/`，git 不跟踪）

| 工具 | 作用 | 用法 |
| --- | --- | --- |
| `repro-secret-channel.mjs` | 假桌面 + 真 host + 假扩展，验证 host 全链路 | `node test-results/repro-secret-channel.mjs <host.exe路径>`，成功标志 `*** FULL ROUNDTRIP OK ***` |
| `probe-live-host.mjs` | 劫持 endpoint，用假桌面驱动**真 host + 真 Firefox 扩展**，可发任意 `password.*` 命令、可观察扩展事件 | `node test-results/probe-live-host.mjs` |
| `probe-real-desktop.mjs` | 裸客户端探测真桌面管道服务器应答（会被 exe 校验拒绝，但能判断服务器是否活着） | `node test-results/probe-real-desktop.mjs` |

机制要点：endpoint 文件 `%LOCALAPPDATA%\PetalDesk\browser-bridge\secret-endpoint.json` 是唯一注入点（桌面每 60s 重写并轮换 token）；探针会备份并自动恢复；运行期间真 host 会连上探针的假管道，按 PID 区分忽略即可。

日志/诊断出口：

- `%LOCALAPPDATA%\PetalDesk\browser-bridge\host-diagnostics.log`：host 侧单行 JSON（handshake/worker-exit/fatal 等），无秘密。
- 密码管理器窗口 → "连接诊断"折叠区块 → "复制诊断信息"：桌面端 bridge 诊断环（连接建立/退休原因/请求超时/probe 结果 + **0.7.2 起新增 badge/fill 业务事件**，见 §5）。
- 桌面端状态接口 `get_password_browser_status` 含 `stdioConnected`/`pipeConnected`/`diagnostics`/`lastRequestOutcome`/`extensionVersion`。

## 5. 剩余问题（badge 与 popup 填充）

现象（2026-08-06 真机，通道已稳定、连接不断连）：

- 3/4 已通：`password.setCaptureEnabled` 同步成功（检测开关显示已开启）、`openPasswordManager` 事件能唤起窗口、`getStatus` 正常。
- 1 不通：访问 mail.163.com（保险库有 `163mail/starsliao`，origin `https://mail.163.com`）角标不显示数字，弹窗显示"此站点暂无已存账户"。
- 2 不通：弹窗里点账户提示"请在页面中确认填充"，但页面浮层不出现。

已排除的环节（都有真机/探针证据）：

- 扩展能实时发出 `originActive` 事件（探针在管道上抓到多个站点的该事件）。
- 扩展对 `password.updateBadge`、`password.setCaptureEnabled`、`password.getStatus`、`ping` 均 50-100ms 正常应答（探针直连真扩展测过）。
- 桌面端 `handle_event` 分发正常（openManager 事件有效）。
- 两端 origin 规范化一致（`url.origin()` vs JS `url.origin`，均为 `https://mail.163.com`）。
- 桌面→扩展命令通道、扩展→桌面事件通道均通。

最可疑的剩余环节（按优先级）：

1. **`handle_origin_active`（`src-tauri/src/password_browser.rs:911`）静默早退或匹配为空**：`badge_accounts()`（:1579）里 `require_any_epoch()` + `list_entries_at()` + `entry.origin == origin` 精确匹配。如果浏览器后台会话 epoch 失效或 vault 锁定，会推 `locked:true`（弹窗应显示"已锁定"而非"暂无账户"——用户看到的是后者，所以更可能是根本没推或匹配为空）。
2. **`push_badge` 的 `password.updateBadge` 推送**（:954）：推了但扩展端 `setBadgeText` 失败，或推到了错误的 connection（多连接时）。
3. **填充链路**：弹窗 `petaldesk.popup.fill` → `fillRequest` 事件 → `handle_fill_request`（:816）→ `password.offerFillDirect` → content 浮层。用户看到"请在页面中确认填充"说明弹窗已接受并发出事件；浮层没出现说明 offerFillDirect 没到 content 或 content 侧渲染失败。注意弹窗账户列表来自 updateBadge 缓存——如果列表是空的，fill 会先被弹窗侧校验挡掉；两个问题可能是同一个（badge 推送没生效）。

**已为此加了业务诊断点**（在 `password_browser.rs`，走 bridge 的 `record_event`，会出现在"连接诊断"的复制内容里）：

- `badge origin-active tabId=… origin=… locked=… accounts=N`
- `badge push tabId=… ok=…`
- `fill fill-request-rejected … reason=…` / `fill offer-fill-direct … ok=…`

**下一步建议**：用 0.7.2 构建（或本地 `cargo build --release` 后按 §6 部署）让用户复现一次，复制诊断信息即可直接看到是"没收到 originActive / 匹配为空 / 推送失败 / 填充被拒"中的哪一种，然后对症修。

其他候选嫌疑（如果诊断点显示一切正常）：扩展端 `updateBadge` 的 `setBadgeText({tabId})` 在真实 Firefox 的行为；弹窗 `queryActiveTab` 与 updateBadge 缓存的 tabId 是否一致；content script 浮层在该站点的渲染（Shadow DOM 被站点 CSP/样式影响）。

## 6. 本机部署状态（2026-08-06）

- `%LOCALAPPDATA%\PetalDesk\petaldesk-browser-host.exe`：**修复版**（重叠 I/O）。原始 0.7.1 备份为同目录 `petaldesk-browser-host.old-0.7.1` / `.bak-0.7.1`。
- `%LOCALAPPDATA%\PetalDesk\petaldesk.exe`：**修复版桌面端**（重叠 I/O + 新前端 + badge/fill 诊断点）。原版备份 `petaldesk.prev` / `petaldesk.old-0.7.1`。
- 部署手法：进程占用 exe 时先 `mv` 改名（Windows 允许重命名运行中的 exe）再 `cp` 新文件；重启桌面端即生效；Firefox 扩展会在重连时自动拉起新 host。
- Firefox 扩展：about:debugging **临时加载**的 `browser-extension/dist/firefox`（= 0.7.1 代码，当前最新；0.7.2 无扩展侧改动）。重启 Firefox 后需重新加载。
- 版本状态：0.7.2 已发布（包含管道修复与扩展 origin 修复）。

## 7. 复现/验证清单

1. `cargo test --manifest-path src-tauri/Cargo.toml`（当前 349+ 通过）。
2. `node test-results/repro-secret-channel.mjs src-tauri/target/debug/petaldesk-browser-host.exe` → `FULL ROUNDTRIP OK`。
3. 真机：部署后 host 日志应长时间无 `worker-exit`/handshake 刷屏；角标/弹窗/填充/检测四项手工验证。

## 8. 安全边界（修复全程未触碰）

密码只走内存管道；诊断只记命令名/请求 ID/原因/来源 origin，不含用户名、密码、token；填充仍需页面浮层二次确认；HTTP 逐 origin 显式允许；会话/连接/tab/frame/document/origin 绑定校验全部保留。
