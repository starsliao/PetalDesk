# 本地存储设计

飞花 - PetalDesk 面向单机、读取频繁、写入较少的桌面场景。坚果云、OneDrive 等同步目录只是可选的文件搬运层，不是产品的核心同步协议。因此存储层保持简单：本地文件是唯一真相，不引入 CRDT、分布式锁、逐操作日志或常驻同步数据库。

## 数据粒度

| 数据 | 权威格式 | 保存粒度 | 原因 |
| --- | --- | --- | --- |
| 便签 | 每条 `note.md + meta.json + assets/` | 单条便签 | 外部编辑友好，修改一条便签不会重写全部数据 |
| 甘特图 | `tools/gantt/gantt.json` | 整体快照 | 任务量小且排序、删除相互关联，整体快照更容易保证一致性 |
| MFA | `tools/mfa/vault.json` | 整体加密保险库 | 账户量小，单个 AEAD 密文便于完整认证；平台本机保护与恢复密码包装支持免密使用和跨机迁移 |

SQLite FTS 只保存可重建的搜索索引，位于 Windows 的 `%LOCALAPPDATA%/PetalDesk` 或 macOS 的 `~/Library/Application Support/PetalDesk`，不属于需要迁移的权威数据。

甘特图快照只包含文档标识、版本、更新时间和当前任务，不保存删除墓碑。删除墓碑只有在真正的多端增量同步协议中才有意义，在当前本地低写入场景中只会无限累积。

## 保存原则

1. 普通读取不写文件；只有显式修改、旧格式迁移或确认到外部正文变化时才落盘。
2. 权威文件先写同目录临时文件并刷新到磁盘，再通过当前平台的原子替换发布。
3. 便签提交校验 `revision + contentHash`；甘特图和 MFA 保存前校验上次读取的磁盘哈希。
4. 基线不一致时拒绝覆盖，并把当前待保存版本写入 `conflicts/` 供人工恢复。
5. 便签、甘特图和 MFA 都保留有限数量的历史备份，避免备份目录无限增长。
6. 搜索索引失败只标记为待重建，不能让正文保存失败。
7. 受管子目录解析后必须仍位于所选数据目录，不跟随指向外部位置的 junction 或符号链接。

这种方案不能承诺多台电脑同时编辑后自动合并；它承诺的是不静默覆盖，并留下两份可辨认的数据。对于本产品的低写入场景，这比引入完整同步引擎更可预测。

## 参考与取舍

- [Joplin 同步规范](https://github.com/laurent22/joplin/blob/dev/readme/dev/spec/sync.md) 将本地数据作为离线优先基础，并按独立 item 同步；飞花借鉴便签按条目拆分，但不实现它的服务器轮询和 `sync_items` 状态机。
- [Joplin 冲突说明](https://github.com/laurent22/joplin/blob/dev/readme/apps/conflict.md) 在无法自动判断时保留本地冲突副本；飞花采用同样的“不丢任一版本”原则。
- [Obsidian 冲突策略](https://github.com/obsidianmd/obsidian-help/blob/master/en/Obsidian%20Sync/Sync%20settings%20and%20selective%20syncing.md#conflict-resolution) 支持生成独立冲突文件供人工检查；这与飞花的文件夹存储方式匹配。
- [Trilium 内容哈希](https://github.com/TriliumNext/Notes/blob/develop/docs/Developer%20Guide/Developer%20Guide/Development%20and%20architecture/Synchronisation/Content%20hashing.md) 使用内容哈希校验同步状态；飞花只在本地保存基线与索引增量判断中使用 SHA-256。
- [Trilium 备份](https://github.com/TriliumNext/Notes/blob/develop/docs/User%20Guide/User%20Guide/Installation%20%26%20Setup/Backup.md) 采用有限数量的周期快照；飞花同样限制备份数量，但在低频写入前创建快照。
- [Aegis](https://github.com/beemdevelopment/Aegis) 采用加密保险库和可恢复备份；飞花 MFA 也保持单一认证加密保险库，并通过恢复密码为复制到新设备的保险库提供显式恢复路径。

Joplin、Trilium 和 Super Productivity 的完整同步引擎适用于多设备持续协作，但会引入服务器状态、删除墓碑、同步队列、重试和冲突合并。当前需求并不需要这些成本；若未来多设备同步成为主场景，应单独设计同步协议，而不是让第三方同步盘承担数据库一致性。

## 迁移

复制整个飞花数据存储目录即可迁移普通数据，然后在飞花设置中选择新目录。旧布局可运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\migrate-petaldesk-storage.ps1 `
  -SourceRoot "旧目录" -TargetRoot "新目录"
```

迁移真实目录前必须从托盘显式退出飞花。脚本按 SHA-256 判断相同文件，目标存在不同内容时停止并生成 JSON 报告，不覆盖任何一边。

首次使用 MFA 时必须先设置恢复密码，完成后才能写入账户。保险库的随机数据密钥始终保留 Argon2id 恢复密码包装，并按使用过的平台保留本机免密包装：Windows 使用当前用户的 DPAPI，macOS 使用当前用户的 Keychain。恢复密码只参与密钥解包，不直接加密账户正文，也不会写入磁盘。

复制完整数据目录后，在新电脑、另一个系统用户或另一平台首次打开 MFA 时输入一次恢复密码，飞花会先验证保险库，再只为当前平台重新绑定本机保护，后续继续免密打开。已有的 Windows DPAPI 包装或 macOS Keychain 标识会在另一平台写入、备份和冲突副本中原样保留，不会因迁移而被删除。每份 MFA 备份都包含恢复密码包装，因此可随主保险库一起迁移和恢复。Keychain 中只保存用于本机解锁的受保护材料；账户正文仍只存在于数据目录的认证加密保险库中。
