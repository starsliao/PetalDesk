# 网站与版本发布

## 产品网站

网站源码位于 `website/`，其中 `index.html` 和 `assets/` 可以作为一个完整静态站点直接打开。推送影响 `website/**` 的 `main` 分支提交后，`.github/workflows/pages.yml` 会自动部署到：

<https://starsliao.github.io/PetalDesk/>

## Windows Release

本地生成联网安装包：

```powershell
pnpm package:windows
```

安装包输出到 `src-tauri/target/release/bundle/nsis/`，构建目录不会提交到 Git。推送 `v*` 标签后，`.github/workflows/release.yml` 会在 Windows Runner 中重新构建，并把安装包发布到 GitHub Releases。

当前版本页面：

<https://github.com/starsliao/PetalDesk/releases/tag/v0.2.1>
