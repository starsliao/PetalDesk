# AMO 公开上架文案（zh-CN 草稿）

## 名称

飞花 - PetalDesk 浏览器增强

## 简短摘要

连接本机飞花桌面应用，在 Firefox 中完成长截图，并在用户逐次确认后安全填充或保存站点账号密码。

## 完整介绍

飞花浏览器增强是飞花 - PetalDesk Windows 桌面应用的 Firefox 配套扩展。

主要功能：

- 对网页滚动区域执行长截图，保持现有飞花长截图工作流。
- 从飞花密码管理器打开登录页，并在页面浮层中再次确认后填写用户名和密码。
- 识别用户主动提交的登录表单，在判断为成功登录后提示新增或更新飞花中的账户。
- 浏览网站时在工具栏角标显示该站点已保存的账户数；点击图标弹出连接诊断与当前站点账户列表，点击账户可在当前页发起填充（仍需页面浮层二次确认）。
- 允许用户为改版或内部站点录制受约束的字段模板；录制只保存选择器，不读取字段内容。
- 支持通用登录表单，以及 Google、Microsoft、阿里云、腾讯云和华为云的内置字段模板。

安全边界：

- 扩展必须连接本机已注册的飞花 Native Messaging Host；不会把数据发送到飞花或其他第三方服务器。
- 用户名和密码数据权限（authenticationInfo）为安装时一次性授予的必要权限；扩展不在运行时二次申请，也没有工具栏授权步骤。
- 每次填充前都会在目标网页显示确认浮层；扩展只填写字段，绝不自动提交表单。
- 登录候选只在扩展内存中短暂保留，超时、断开连接或页面关闭后立即清除，不写入浏览器存储。
- HTTP 站点默认拒绝密码功能，必须在飞花中逐站点明确允许并持续显示风险提示。

密码保险库位于用户自己的 Windows 设备上，由飞花桌面应用加密保存。扩展不能独立读取保险库。

## 分类与标签建议

- 主分类：Privacy & Security
- 备选分类：Other
- 标签：password manager, productivity, screenshot, local-first, PetalDesk

## 首版更新说明

- 保留飞花网页长截图能力。
- 新增经用户确认的密码填充和登录信息检测。
- 身份验证信息数据权限改为安装时授予的必要权限，无运行时二次授权。
- 新增工具栏角标账户数与带连接诊断的工具栏弹层。
- 新增 Google、Microsoft、阿里云、腾讯云和华为云登录模板。
- 新增按精确站点 origin 绑定的用户模板录制。

## 提交前待补信息

- 发布者显示名：`<PUBLISHER_NAME>`
- 支持邮箱：`<SUPPORT_EMAIL>`
- 支持页面：https://github.com/starsliao/PetalDesk/issues
- 项目主页：https://starsliao.github.io/PetalDesk/
- 隐私政策公开 URL：https://starsliao.github.io/PetalDesk/privacy.html
- Windows 安装包公开 URL：https://github.com/starsliao/PetalDesk/releases/download/v0.7.1/PetalDesk_0.7.1_x64-setup.exe
- 许可证：MIT
