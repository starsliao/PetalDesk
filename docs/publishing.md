# 网站与版本发布

## 产品网站

网站源码位于 `website/`，其中 `index.html` 和 `assets/` 可以作为一个完整静态站点直接打开。推送影响 `website/**` 的 `main` 分支提交后，`.github/workflows/pages.yml` 会自动部署到：

<https://starsliao.github.io/PetalDesk/>

## Windows Release

本地生成联网安装包：

```powershell
pnpm package:windows
```

Version `0.3.0` adds scrolling long capture. The generic capture path works
without a browser extension; the Chrome, Edge, and Firefox extensions provide
browser-assisted scrolling and page-state coordination for more reliable long
page capture.

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

安装包输出到 `src-tauri/target/release/bundle/nsis/`，构建目录不会提交到 Git。推送 `v*` 标签后，`.github/workflows/release.yml` 会在 Windows Runner 中重新构建，并把安装包发布到 GitHub Releases。

当前版本页面：

<https://github.com/starsliao/PetalDesk/releases/tag/v0.3.0>
