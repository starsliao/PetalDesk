# 网站与版本发布

## 产品网站

网站源码位于 `website/`，其中 `index.html` 和 `assets/` 可以作为一个完整静态站点直接打开。推送影响 `website/**` 的 `main` 分支提交后，`.github/workflows/pages.yml` 会自动部署到：

<https://starsliao.github.io/PetalDesk/>

## Windows Release

本地生成联网安装包：

```powershell
pnpm package:windows
```

Version `0.4.3` requires the current recovery password before rotating the MFA
recovery password, compacts the authenticated MFA export dialog without a
vertical scrollbar, shortens the first tray menu item to `打开 飞花`, and moves
`新建便签` into the second position.
Version `0.4.2` adds line-oriented batch URI import for MFA accounts and assigns
varied default icons to batch-added accounts. Individual MFA accounts can now be
exported as a Base32 secret, complete `otpauth://` URI, and QR code after recovery
password verification. Deleted MFA accounts move to the encrypted trash, where
they can be restored; permanent deletion and emptying the trash require a second
confirmation. Notes also copy selected text automatically. This release lets
desktop activation reuse the configurable tray double-click action, adds a
recent-note target, and fixes screenshot-window focus when the shortcut is
pressed inside PetalDesk.
Version `0.4.1` added configurable tray double-click actions plus manual ordering and pinning for MFA accounts.
Version `0.4.0` introduced portable MFA recovery
and hardened local data migration. The MFA tool supports standard
RFC 6238 TOTP accounts, screen/image/URI/manual import, hidden-by-default codes,
and safe clipboard cleanup. The encrypted vault keeps a random data key behind
two independent wrappers: Windows DPAPI for passwordless local use and an
Argon2id-derived recovery-password wrapper for migration. After copying the
complete PetalDesk data directory to another computer or Windows user, enter the
recovery password once; the vault then creates a new local DPAPI wrapper.

The package always includes and registers the Firefox Native Messaging host.
Set `PETALDESK_CHROME_EXTENSION_ID` and/or `PETALDESK_EDGE_EXTENSION_ID` to the
32-character store extension IDs when producing Chromium-enabled releases.
Missing Chromium IDs skip only that browser registration.
The GitHub tag workflow reads the same values from repository secrets named
`PETALDESK_CHROME_EXTENSION_ID` and `PETALDESK_EDGE_EXTENSION_ID`.
The installer does not install the browser extension itself. A public Firefox
release must also publish an AMO-signed build from `browser-extension/dist/firefox`;
the manifest's stable Gecko ID already matches the registered host allowlist.
Chrome and Edge share the Chromium extension build, while Firefox uses its own
manifest and signed XPI.

Generate the versioned AMO upload archive locally with:

```powershell
npm --prefix browser-extension run package:firefox
```

The tag workflow always attaches this clearly named unsigned AMO upload ZIP.
When repository secrets `AMO_JWT_ISSUER` and `AMO_JWT_SECRET` are both present,
it also requests an unlisted AMO signature and attaches the resulting signed
XPI. The unsigned ZIP must not be presented as an installable Firefox package.

The `0.4.3` installer can be installed directly over an earlier PetalDesk
version. On first launch it migrates legacy note metadata and the previous
Gantt JSON layout into `.petaldesk/`, preserving the note Markdown and a
migration backup. Same-machine upgrades do not require an MFA recovery
password; that password is used when the encrypted data directory moves to a
different Windows user or computer, and when a user explicitly exports one MFA
account.

安装包输出到 `src-tauri/target/release/bundle/nsis/`，构建目录不会提交到 Git。推送 `v*` 标签后，`.github/workflows/release.yml` 会在 Windows Runner 中重新构建，并把安装包发布到 GitHub Releases。

当前版本页面：

<https://github.com/starsliao/PetalDesk/releases/tag/v0.4.3>
