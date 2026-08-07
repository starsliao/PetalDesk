<script lang="ts">
  import { onMount } from "svelte";
  import {
    AlertCircle,
    CheckCircle2,
    Download,
    LoaderCircle,
    RefreshCw,
    RotateCcw,
    X,
  } from "@lucide/svelte";
  import {
    updaterApi,
    type UpdateApi,
    type UpdateSettings,
    type UpdateState,
  } from "$lib/updater";

  interface Props {
    currentVersion?: string;
    buildTimestamp?: number;
    supported?: boolean;
    api?: UpdateApi;
    onclose?: () => void;
  }

  let {
    currentVersion = "",
    buildTimestamp,
    supported = updaterApi.isSupported(),
    api = updaterApi,
    onclose,
  }: Props = $props();

  let settings = $state<UpdateSettings>({ autoUpdate: true });
  let updateState = $state<UpdateState>({
    phase: "idle",
    currentVersion: "",
    availableVersion: null,
    releaseNotes: null,
    publishedAt: null,
    downloadedBytes: 0,
    totalBytes: null,
    error: null,
  });
  let loading = $state(true);
  let settingsBusy = $state(false);
  let actionBusy = $state(false);
  let actionError = $state<string | null>(null);

  let displayedVersion = $derived(updateState.currentVersion || currentVersion || "未知");
  let displayedBuild = $derived(formatBuildTimestamp(buildTimestamp));
  let working = $derived(
    actionBusy
      || updateState.phase === "checking"
      || updateState.phase === "downloading"
      || updateState.phase === "installing",
  );
  let progress = $derived(
    updateState.totalBytes
      ? Math.min(100, Math.max(0, updateState.downloadedBytes / updateState.totalBytes * 100))
      : null,
  );

  function message(error: unknown): string {
    if (typeof error === "object" && error && "message" in error) return String(error.message);
    return typeof error === "string" ? error : "更新操作失败，请稍后重试。";
  }

  function formatBytes(bytes: number | null): string {
    if (!bytes || bytes <= 0) return "";
    if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
    if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
    return `${bytes} B`;
  }

  function formatBuildTimestamp(timestamp: number | undefined): { iso: string; label: string } | null {
    if (!Number.isFinite(timestamp) || !timestamp || timestamp <= 0) return null;
    const date = new Date(timestamp * 1000);
    if (Number.isNaN(date.getTime())) return null;
    const pad = (value: number): string => String(value).padStart(2, "0");
    return {
      iso: date.toISOString(),
      label: `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`,
    };
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape" || updateState.phase === "installing") return;
    event.preventDefault();
    onclose?.();
  }

  async function setAutoUpdate(event: Event): Promise<void> {
    if (!supported || settingsBusy) return;
    const next = (event.currentTarget as HTMLInputElement).checked;
    const previous = settings.autoUpdate;
    settings = { autoUpdate: next };
    settingsBusy = true;
    actionError = null;
    try {
      settings = await api.setSettings({ autoUpdate: next });
    } catch (error) {
      settings = { autoUpdate: previous };
      actionError = message(error);
    } finally {
      settingsBusy = false;
    }
  }

  async function runAction(action: () => Promise<UpdateState>): Promise<boolean> {
    if (!supported || actionBusy) return false;
    actionBusy = true;
    actionError = null;
    try {
      updateState = await action();
      return true;
    } catch (error) {
      const detail = message(error);
      actionError = detail;
      updateState = { ...updateState, phase: "error", error: detail };
      return false;
    } finally {
      actionBusy = false;
    }
  }

  async function checkNow(): Promise<void> {
    updateState = { ...updateState, phase: "checking", error: null };
    await runAction(() => api.check());
  }

  async function downloadNow(): Promise<void> {
    updateState = { ...updateState, phase: "downloading", error: null };
    await runAction(() => api.download());
  }

  async function installNow(): Promise<void> {
    if (!supported || actionBusy) return;
    actionBusy = true;
    actionError = null;
    updateState = { ...updateState, phase: "installing", error: null };
    try {
      await api.installAndRestart();
    } catch (error) {
      actionError = message(error);
      updateState = { ...updateState, phase: "ready" };
      actionBusy = false;
    }
  }

  async function postpone(): Promise<void> {
    if (await runAction(() => api.postpone())) onclose?.();
  }

  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    if (!supported) {
      loading = false;
      return;
    }

    void (async () => {
      try {
        unlisten = await api.listen((state) => {
          if (!disposed) updateState = state;
        });
        const [nextSettings, nextState] = await Promise.all([
          api.getSettings(),
          api.getState(),
        ]);
        if (!disposed) {
          settings = nextSettings;
          updateState = nextState;
        }
      } catch (error) {
        if (!disposed) actionError = message(error);
      } finally {
        if (!disposed) loading = false;
      }
    })();

    return () => {
      disposed = true;
      unlisten?.();
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="about-backdrop">
  <button
    type="button"
    class="backdrop-dismiss"
    aria-label="关闭关于与更新"
    disabled={updateState.phase === "installing"}
    onclick={onclose}
  ></button>
  <div class="about-dialog" role="dialog" aria-modal="true" aria-labelledby="about-title">
    <header>
      <div class="product">
        <img src="/app-icon.svg" alt="" />
        <div>
          <h2 id="about-title">飞花 - PetalDesk</h2>
          <p>轻巧、本地优先的 Markdown 便签与桌面小工具</p>
        </div>
      </div>
      <button
        type="button"
        class="icon-button"
        aria-label="关闭"
        disabled={updateState.phase === "installing"}
        onclick={onclose}
      >
        <X size={18} aria-hidden="true" />
      </button>
    </header>

    <div class="version-line">
      <span>当前版本</span>
      <strong>v{displayedVersion}</strong>
      {#if displayedBuild}
        <time class="build-time" datetime={displayedBuild.iso}>打包时间 {displayedBuild.label}</time>
      {/if}
    </div>

    <section class="update-section" aria-labelledby="update-title">
      <div class="section-heading">
        <div>
          <h3 id="update-title">软件更新</h3>
          <p>更新只替换程序文件，不会修改飞花数据存储。</p>
        </div>
        {#if supported}
          <button
            type="button"
            class="check-button"
            disabled={loading || working || updateState.phase === "ready"}
            onclick={() => void checkNow()}
          >
            {#if updateState.phase === "checking"}
              <LoaderCircle class="spinner" size={15} aria-hidden="true" />
              检查中…
            {:else}
              <RefreshCw size={15} aria-hidden="true" />
              检查更新
            {/if}
          </button>
        {/if}
      </div>

      {#if supported}
        <label class="auto-update-row">
          <input
            type="checkbox"
            checked={settings.autoUpdate}
            disabled={loading || settingsBusy || updateState.phase === "installing"}
            onchange={(event) => void setAutoUpdate(event)}
          />
          <span>
            <strong>自动检查并下载更新</strong>
            <small>默认开启；下载完成后由你选择何时重启安装。</small>
          </span>
        </label>

        <div class:has-error={Boolean(actionError || updateState.error)} class="update-status" aria-live="polite">
          {#if loading}
            <LoaderCircle class="spinner" size={19} aria-hidden="true" />
            <div><strong>正在读取更新设置…</strong></div>
          {:else if updateState.phase === "checking"}
            <LoaderCircle class="spinner" size={19} aria-hidden="true" />
            <div><strong>正在检查新版本</strong><span>这通常只需要几秒钟。</span></div>
          {:else if updateState.phase === "upToDate"}
            <CheckCircle2 class="success-icon" size={19} aria-hidden="true" />
            <div><strong>已经是最新版本</strong><span>当前使用 v{displayedVersion}。</span></div>
          {:else if updateState.phase === "available"}
            <Download class="accent-icon" size={19} aria-hidden="true" />
            <div class="status-main">
              <strong>发现新版本 v{updateState.availableVersion ?? ""}</strong>
              {#if updateState.totalBytes}<span>安装包约 {formatBytes(updateState.totalBytes)}</span>{/if}
            </div>
            <button type="button" class="primary-button compact" disabled={working} onclick={() => void downloadNow()}>
              下载更新
            </button>
          {:else if updateState.phase === "downloading"}
            <Download class="accent-icon" size={19} aria-hidden="true" />
            <div class="status-main download-status">
              <strong>正在下载 v{updateState.availableVersion ?? "新版本"}</strong>
              <div class:indeterminate={progress === null} class="progress-track" role="progressbar" aria-label="更新下载进度" aria-valuemin="0" aria-valuemax="100" aria-valuenow={progress === null ? undefined : Math.round(progress)}>
                <span style:width={progress === null ? "36%" : `${progress}%`}></span>
              </div>
              <span>
                {formatBytes(updateState.downloadedBytes)}{updateState.totalBytes ? ` / ${formatBytes(updateState.totalBytes)}` : ""}
                {progress === null ? "" : ` · ${Math.round(progress)}%`}
              </span>
            </div>
          {:else if updateState.phase === "ready"}
            {#if actionError || updateState.error}
              <AlertCircle size={19} aria-hidden="true" />
            {:else}
              <CheckCircle2 class="success-icon" size={19} aria-hidden="true" />
            {/if}
            <div class="status-main">
              <strong>v{updateState.availableVersion ?? "新版本"} 已准备好</strong>
              <span>{actionError || updateState.error || "重启前会先安全保存便签和小工具数据。"}</span>
            </div>
            <div class="ready-actions">
              <button type="button" class="secondary-button compact" disabled={working} onclick={() => void postpone()}>稍后</button>
              <button type="button" class="primary-button compact" disabled={working} onclick={() => void installNow()}>
                <RotateCcw size={14} aria-hidden="true" />
                立即重启更新
              </button>
            </div>
          {:else if updateState.phase === "installing"}
            <LoaderCircle class="spinner" size={19} aria-hidden="true" />
            <div><strong>正在安全保存并准备更新…</strong><span>飞花稍后将自动重启。</span></div>
          {:else if updateState.phase === "error" || actionError || updateState.error}
            <AlertCircle size={19} aria-hidden="true" />
            <div><strong>暂时无法完成更新</strong><span>{actionError || updateState.error}</span></div>
          {:else}
            <RefreshCw class="muted-icon" size={19} aria-hidden="true" />
            <div><strong>自动更新已就绪</strong><span>你也可以随时手动检查。</span></div>
          {/if}
        </div>

        {#if updateState.releaseNotes && ["available", "downloading", "ready"].includes(updateState.phase)}
          <details class="release-notes" open={updateState.phase === "ready"}>
            <summary>v{updateState.availableVersion ?? "新版本"} 更新内容</summary>
            <div>{updateState.releaseNotes}</div>
          </details>
        {/if}
      {:else}
        <div class="unsupported-status">
          <AlertCircle size={19} aria-hidden="true" />
          <div>
            <strong>自动更新第一阶段仅支持 Windows</strong>
            <span>其他平台请通过 GitHub Releases 获取新版本。</span>
          </div>
        </div>
      {/if}
    </section>

    <footer>
      <span>© 飞花 - PetalDesk · 本地优先，数据由你掌控</span>
      <button type="button" class="secondary-button" disabled={updateState.phase === "installing"} onclick={onclose}>关闭</button>
    </footer>
  </div>
</div>

<style>
  .about-backdrop {
    position: fixed;
    z-index: 1100;
    inset: 0;
    display: grid;
    padding: 18px;
    place-items: center;
    background: rgb(0 0 0 / 30%);
    backdrop-filter: blur(2px);
  }

  .backdrop-dismiss {
    position: absolute;
    inset: 0;
    padding: 0;
    background: transparent;
    border: 0;
  }

  .about-dialog {
    position: relative;
    width: min(100%, 570px);
    max-height: min(720px, calc(100vh - 36px));
    overflow: auto;
    color: var(--app-fg);
    background: var(--app-surface);
    border: 1px solid var(--app-border);
    border-radius: 8px;
    box-shadow: 0 20px 70px rgb(0 0 0 / 26%);
  }

  header,
  footer,
  .product,
  .version-line,
  .section-heading,
  .auto-update-row,
  .update-status,
  .unsupported-status,
  .ready-actions,
  .check-button,
  .primary-button {
    display: flex;
    align-items: center;
  }

  header {
    padding: 20px 20px 16px;
    justify-content: space-between;
    gap: 16px;
  }

  .product {
    min-width: 0;
    gap: 13px;
  }

  .product img {
    width: 46px;
    height: 46px;
    flex: 0 0 auto;
    filter: drop-shadow(0 4px 9px rgb(0 0 0 / 13%));
  }

  h2,
  h3,
  p {
    margin: 0;
  }

  h2 {
    font-size: 18px;
    line-height: 1.35;
  }

  .product p {
    margin-top: 3px;
    color: var(--app-muted);
    font-size: 12.5px;
    line-height: 1.4;
  }

  .icon-button {
    display: grid;
    width: 32px;
    height: 32px;
    flex: 0 0 auto;
    padding: 0;
    place-items: center;
    color: var(--app-muted);
    background: transparent;
    border: 0;
    border-radius: 5px;
  }

  .icon-button:hover:not(:disabled) {
    color: var(--app-fg);
    background: var(--app-surface-hover);
  }

  .version-line {
    min-height: 38px;
    padding: 7px 20px;
    flex-wrap: wrap;
    gap: 8px;
    color: var(--app-muted);
    font-size: 12px;
    background: linear-gradient(90deg, rgb(0 103 192 / 7%), transparent 72%);
    border-block: 1px solid var(--app-border);
  }

  .version-line strong {
    color: var(--app-fg);
    font-size: 13px;
  }

  .build-time {
    margin-left: auto;
    color: var(--app-muted);
    font-variant-numeric: tabular-nums;
  }

  .update-section {
    padding: 18px 20px 20px;
  }

  .section-heading {
    justify-content: space-between;
    gap: 16px;
  }

  h3 {
    font-size: 14px;
  }

  .section-heading p {
    margin-top: 3px;
    color: var(--app-muted);
    font-size: 11.5px;
  }

  .check-button {
    min-height: 32px;
    padding: 5px 10px;
    flex: 0 0 auto;
    justify-content: center;
    gap: 6px;
    color: var(--app-accent);
    font-size: 12px;
    font-weight: 650;
    background: #fff;
    border: 1px solid var(--app-border-strong);
    border-radius: 5px;
  }

  .check-button:hover:not(:disabled) {
    background: rgb(0 103 192 / 6%);
    border-color: var(--app-accent);
  }

  .auto-update-row {
    margin-top: 16px;
    padding: 11px 12px;
    align-items: flex-start;
    gap: 10px;
    background: #f8f8f8;
    border: 1px solid var(--app-border);
    border-radius: 6px;
    cursor: pointer;
  }

  .auto-update-row input {
    width: 16px;
    height: 16px;
    margin: 2px 0 0;
    accent-color: var(--app-accent);
  }

  .auto-update-row span,
  .update-status > div,
  .unsupported-status > div {
    display: grid;
    min-width: 0;
    gap: 3px;
  }

  .auto-update-row strong,
  .update-status strong,
  .unsupported-status strong {
    font-size: 12.5px;
  }

  .auto-update-row small,
  .update-status span,
  .unsupported-status span {
    color: var(--app-muted);
    font-size: 11.5px;
    line-height: 1.45;
  }

  .update-status,
  .unsupported-status {
    min-height: 66px;
    margin-top: 11px;
    padding: 11px 12px;
    align-items: flex-start;
    gap: 10px;
    color: var(--app-fg);
    background: #f7fbff;
    border: 1px solid #d0e3f3;
    border-radius: 6px;
  }

  .update-status > :global(svg),
  .unsupported-status > :global(svg) {
    flex: 0 0 auto;
    margin-top: 1px;
  }

  .update-status.has-error {
    color: #8f1d14;
    background: #fff3f1;
    border-color: #f0c4be;
  }

  .status-main {
    flex: 1 1 auto;
  }

  .ready-actions {
    flex: 0 0 auto;
    align-self: center;
    gap: 6px;
  }

  :global(.success-icon) {
    color: #178049;
  }

  :global(.accent-icon) {
    color: var(--app-accent);
  }

  :global(.muted-icon) {
    color: var(--app-muted);
  }

  .download-status {
    width: 100%;
  }

  .progress-track {
    position: relative;
    height: 6px;
    margin: 4px 0 1px;
    overflow: hidden;
    background: #dbe5ed;
    border-radius: 999px;
  }

  .progress-track span {
    display: block;
    height: 100%;
    background: var(--app-accent);
    border-radius: inherit;
    transition: width 180ms ease-out;
  }

  .progress-track.indeterminate span {
    animation: indeterminate 1.2s ease-in-out infinite;
  }

  .release-notes {
    margin-top: 11px;
    color: var(--app-fg);
    font-size: 12px;
    background: #fff;
    border: 1px solid var(--app-border);
    border-radius: 6px;
  }

  .release-notes summary {
    padding: 9px 11px;
    font-weight: 650;
    cursor: pointer;
  }

  .release-notes div {
    max-height: 150px;
    padding: 10px 12px;
    overflow: auto;
    line-height: 1.55;
    white-space: pre-wrap;
    border-top: 1px solid var(--app-border);
  }

  .unsupported-status {
    margin-top: 16px;
    background: #fafafa;
    border-color: var(--app-border);
  }

  footer {
    min-height: 58px;
    padding: 11px 20px;
    justify-content: space-between;
    gap: 14px;
    color: var(--app-muted);
    font-size: 11px;
    background: #fafafa;
    border-top: 1px solid var(--app-border);
  }

  .secondary-button,
  .primary-button {
    min-height: 32px;
    padding: 5px 12px;
    justify-content: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 650;
    border-radius: 5px;
  }

  .secondary-button {
    color: var(--app-fg);
    background: #fff;
    border: 1px solid var(--app-border-strong);
  }

  .primary-button {
    color: #fff;
    background: var(--app-accent);
    border: 1px solid #00589f;
  }

  .secondary-button:hover:not(:disabled) {
    background: var(--app-surface-hover);
  }

  .primary-button:hover:not(:disabled) {
    background: #005aab;
  }

  .compact {
    min-height: 29px;
    padding: 4px 8px;
    font-size: 11.5px;
  }

  button:disabled,
  input:disabled {
    cursor: not-allowed;
    opacity: 0.58;
  }

  :global(.spinner) {
    animation: spin 0.9s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  @keyframes indeterminate {
    from { transform: translateX(-110%); }
    to { transform: translateX(320%); }
  }

  @media (max-width: 540px) {
    .about-backdrop {
      padding: 8px;
    }

    header,
    .update-section,
    footer {
      padding-inline: 14px;
    }

    .version-line {
      padding-inline: 14px;
    }

    .build-time {
      width: 100%;
      margin-left: 0;
    }

    .section-heading,
    .update-status {
      align-items: stretch;
      flex-direction: column;
    }

    .check-button {
      width: 100%;
    }

    .ready-actions {
      width: 100%;
      align-self: stretch;
    }

    .ready-actions button {
      flex: 1 1 0;
    }

    footer span {
      display: none;
    }

    footer {
      justify-content: flex-end;
    }
  }
</style>
