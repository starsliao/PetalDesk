**飞花密码管理器与浏览器自动填充**

**Summary**

新增 Windows 首发的密码管理器，并把填充与登录信息检测合并到现有长截图扩展。首版只在普通 Firefox 中提供扩展自动填充；无扩展时降级为打开网址和复制凭据，Chrome/Edge 留待后续版本。

**首版范围与发布目标**

- 首版浏览器只支持 Firefox Desktop；Chrome 和 Edge 的 manifest、商店提交、Native Messaging ID 和安装验收全部延期，不作为首版交付条件。
- 首版扩展继续使用固定 Gecko ID `petaldesk-capture@petaldesk.app`，保留长截图协议兼容性；协议和后端边界保持可扩展，后续再增加 Chromium 构建。
- 首版选择 **AMO 公开上架（listed）**，不是 self-distribution/unlisted。公开上架后用户可从 AMO 详情页安装，Firefox 会为已安装版本提供后续更新；提交仍需通过自动校验，并可能在发布后进入人工审核。
- AMO 上架不等于桌面安装器可以静默安装扩展。用户仍需在 Firefox 中确认安装；飞花安装器只注册 Firefox Native Messaging Host，并在扩展缺失时给出安装入口。
- “不公开签名（unlisted）”只作为开发测试或审核被拒时的备用分发方式：扩展仍须提交 AMO 签名，不能绕过账号、自动校验或可能的人工审核，也不能出现在 AMO 搜索结果中。

**Firefox AMO 公开上架**

结论：仓库内的扩展实现、Firefox 构建、AMO 校验包、隐私/权限说明草稿、截图和审核测试说明可以由飞花直接完成；以下账号操作和最终发布动作必须由产品所有者完成，不能把账号密码、2FA 恢复码或 API secret 发到聊天中。

用户需要提供或确认：

- 一个可登录 `addons.mozilla.org` 的 Firefox 账号，完成邮箱验证、AMO 页面要求的账号验证/安全挑战（如有），并在 AMO 后台接受开发者协议。账号登录和最终提交由用户本人操作；AMO 当前没有面向普通扩展的统一付费开发者注册要求，但若后台出现额外身份提示，以后台为准。
- 发布者显示名、联系邮箱、支持页面/问题反馈地址、项目主页，以及是否使用现有 GitHub 仓库作为源码和下载入口。
- 最终的扩展名称、短摘要、完整介绍、中文/英文展示语言、分类、标签、版本更新说明、版权/许可证和隐私政策正文/URL。公开 listing 可以把隐私政策放在 AMO；我会同时准备一个稳定的外部网站副本，内容按“本地保存、只通过本机 Native Messaging 传递、不上传远程服务器”的实际行为起草，用户确认法律表述后再提交。
- 品牌素材确认：扩展图标、商店截图、可选宣传图。没有新素材时可以使用飞花现有图标并由我生成 Firefox 界面截图；用户只需确认名称和视觉资产可用于公开发布。
- 审核测试路径：审核员是否可以安装公开的飞花 Windows 安装包，以及密码管理器的无真实账号测试页面/测试数据。不能要求审核员使用真实密码；若必须安装桌面端才能验证 Native Messaging，应提供公开下载链接、可复现步骤和脱敏测试账号或本地 fixture。
- 首版 AMO 公开版本号为 `0.6.1`（桌面端 `0.6.0` 已先行发布）。`package.json`、Cargo、Tauri 和两个浏览器 manifest 必须在提交标签前保持一致；Firefox AMO 首次公开提交也使用此版本号。

我可以直接完成：

- 只保留并验证 Firefox 首版的 manifest、权限、固定 Gecko ID、Native Messaging allowlist 和 Windows 注册流程；不为 Chrome/Edge 做首版发布工作。
- 增加 Firefox 专用的构建/打包检查，生成可上传 AMO 的 ZIP，并用 `web-ext@10.5.0` 执行 lint、测试和版本一致性检查。AMO 公开上架使用 `listed` 通道；现有脚本中的 `unlisted` 只保留为备用/测试路径。
- 按 AMO 的数据披露规则重新审查权限。长截图当前声明 `websiteActivity`；密码管理器检测和填充用户名/密码属于 `authenticationInfo`，必须在最终 manifest 和 AMO 表单中如实说明，不能继续只声明网站活动。
- 准备源码包和可复现构建说明。当前构建是可读的文件复制，不做混淆；仍会准备源代码、依赖版本、`npm test`、`npm run build`、`npm run package:firefox` 和审核启动步骤，避免 AMO 审核员无法复现构建。
- 准备 AMO listing 文案、隐私政策草稿、权限理由、审核员测试说明和截图；用户确认发布者信息后写入最终提交材料。
- 首次提交后根据 AMO 自动校验和人工审核反馈修复问题，再由用户在同一个 AMO listing 下上传更新版本，避免产生新的扩展 ID。

AMO 发布步骤：

1. 用户创建/确认 Firefox 账号，完成邮箱验证、2FA 和开发者协议确认，并把发布者资料提供给飞花。
2. 飞花完成 Firefox 密码管理器功能、权限/数据披露、源码包、listing 素材和审核 fixture；本地执行扩展测试、`web-ext lint`、构建和 Windows Native Messaging 冒烟测试。
3. 飞花生成带版本号的 AMO 上传 ZIP，并在 AMO Developer Hub 选择 **Listing on AMO** 上传；如果 AMO 要求源码包，按审核页面一起上传。
4. 用户检查名称、描述、权限、数据分类、隐私政策和支持信息后执行最终提交。Mozilla 完成自动校验并可能继续人工审核；审核期间不承诺固定时长。
5. 发布后验证 AMO 详情页、Firefox 安装、自动更新、Native Messaging 握手、长截图回归和密码填充/登录检测；以后更新必须从同一 listing 上传。

自动化发布凭据（可选，不是首次手工提交的前置条件）：

- `0.6.1` 首次公开上架只用 AMO Developer Hub 手工上传。标签 CI 只生成 AMO 上传 ZIP 和审核源码 ZIP，不读取 AMO API secret，也不在桌面 Release 成功前占用不可重复的扩展版本号。
- `browser-extension/scripts/package-firefox.ps1 -Sign` 支持显式的 `Listed` 与 `Unlisted` 通道，供独立测试或未来自动化使用。若后续增加 GitHub Actions 自动上传，用户需把 `AMO_JWT_ISSUER` 和 `AMO_JWT_SECRET` 配置为仓库 Secrets；自动上传必须放在桌面 Release 成功后的独立流程中，并验证始终更新同一个 AMO listing。

**Password Vault**

- 新建独立密码保险库，保存站点名称、登录 URL、精确 origin、用户名、密码、备注、模板和时间信息；同一站点支持多个账户。
- 复用 MFA 的加密、DPAPI、原子保存、备份、冲突检测和敏感数据清理模式，但使用独立目录、数据密钥和 AAD。
- 增加共享恢复密码协调器；首次启用时兼容并迁移现有 MFA 恢复设置，修改恢复密码时同步更新 MFA 与密码库。
- 日常由 Windows DPAPI 自动解锁；窗口关闭、显式锁定或默认空闲 15 分钟后清理解密状态、待处理请求和剪贴板。
- 密码管理器提供搜索、增删改、密码生成、短时显示、复制账号/密码、打开并填充、浏览器状态、登录检测授权和模板录制。

**Browser Integration**

- 扩展保留长截图协议并新增 `password-fill`、`password-capture` capability；首版只编译和发布 Firefox manifest，业务协议保持浏览器中立，便于后续接入 Chromium。
- 密码不得写入现有文件 spool；新增带当前用户 ACL、临时令牌和 TTL 的 Windows named pipe，秘密仅以一次性内存消息传递。
- “打开并填充”由扩展创建并绑定新标签页。加载完成后页面浮层显示最终 origin、账户名及“填充/取消”，确认后才获取凭据。
- 只填写字段并触发必要的 `input/change` 事件，绝不自动提交表单。
- 填充请求绑定 session、浏览器、tab、frame、entry 和 origin；错误标签页、未允许的重定向、歧义字段及跨域 iframe 均拒绝。
- HTTP 内网必须逐 origin 显式放行，并在保存、更新和填充时持续显示不安全警告。

**Built-In Templates**

首版内置“办公云服务包”，当前确认的主登录 origin 为：

- Google：`https://accounts.google.com`
- Microsoft 工作或学校账户：`https://login.microsoftonline.com`
- Microsoft 个人账户：`https://login.live.com`
- 阿里云：`https://account.aliyun.com`
- 腾讯云：`https://cloud.tencent.com`
- 华为云：`https://auth.huaweicloud.com`

模板行为：

- Google、Microsoft 按两步登录模板处理账号页和密码页。
- 阿里云、腾讯云、华为云仅处理账号密码登录模式；遇到默认二维码、短信、Passkey 或第三方登录时，可切换到账号密码页，但不尝试绕过验证码或二次验证。
- 模板只使用稳定语义属性、可访问名称和受约束的选择器，不嵌入任意 JavaScript。
- 每个模板带版本、支持的精确 origin 和回归 fixture；官方重定向 origin 必须逐个验证并登记，不使用泛域名通配。
- 匹配优先级为“用户录制覆盖 > 内置模板 > 通用启发式”。
- 内置模板失效时停止模板步骤并尝试通用识别；仍有歧义则提示用户录制覆盖模板，不猜测字段。

**Login Capture**

- 用户首次进入密码管理器时明确授权；授权前扩展不检测登录信息，设置中可关闭。
- 提交后候选凭据只在扩展内存保留短 TTL。
- 优先等待页面导航、登录框消失等成功信号；无法判断时显示低置信度提示，由用户确认刚才登录成功后再保存。
- 同 origin、同用户名且密码不同则提示更新；用户名不同提示新增，并允许手动替换已有账户；完全相同则不提示。
- 支持登录和修改密码表单；飞花未连接或候选超时时直接清除，不写浏览器 storage、磁盘或日志。

**Interfaces**

- 后端类型包括 `PasswordEntrySummary`、敏感的 `PasswordEntryInput`、`PasswordStatus`、填充票据和模板定义；列表与状态响应不含密码。
- 新增状态、列表、增改删、短时 reveal/copy、开始/取消填充、检测授权、模板录制和共享恢复密码命令。
- 浏览器协议增加 `tabReady`、`fillConfirm`、`fillResult`、`captureCandidate`、`saveDecision` 和模板录制消息。
- 保持现有扩展 ID、Native Host 名称和截图兼容性；旧扩展缺少密码 capability 时明确降级。

**Test Plan**

- Rust：保险库、DPAPI、共享恢复迁移与轮换中断恢复、备份冲突、会话失效、剪贴板清理及磁盘/日志无密码。
- 匹配：同账户更新、多账户新增、重复跳过、SSO allowlist、HTTP 未授权拒绝。
- 扩展 fixture：五组内置模板、单页、两步、密码修改、SPA、失败登录、低置信度提示、歧义字段、用户模板覆盖和无自动提交。
- Windows 集成：Firefox 新标签页绑定、六个登录 origin 的实际页面冒烟测试、扩展缺失降级及长截图无回归；Chrome/Edge 集成测试延期。
- 运行 `cargo test`、`pnpm check`、`pnpm test`、扩展测试及 Firefox 构建打包，并完成 Firefox AMO 安装、Native Messaging 握手、自动更新和审核 fixture 验收。

**Boundaries**

- 首版不做自动提交、隐身窗口填充、浏览器密码数据库读取、CSV 明文导出或云同步。
- 首版只发布 Firefox AMO listed 扩展；Chrome/Edge 扩展发布和商店账号配置不在本版本范围内。
- 浏览器扩展仍需用户在 Firefox 中确认安装；Windows 安装器只注册 Firefox Native Messaging Host，不静默安装扩展。
- 页面改版、地区跳转或官方登录域变化必须使模板安全失败，不能放宽 origin 或继续猜测。
- 当前工作区已有用户修改，实施时必须保留并在其基础上合并。
