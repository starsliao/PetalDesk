<script lang="ts">
  import { FileText, FolderOpen, Info, Keyboard, RotateCcw, X } from "@lucide/svelte";
  import {
    DEFAULT_TRAY_SHORTCUT_SETTINGS,
    type TrayShortcutAction,
    type TrayShortcutSettings,
  } from "$lib/bridge";
  import type { EditorMode } from "$lib/editor";
  import { formatShortcut } from "$lib/shortcuts";

  interface Props {
    open?: boolean;
    shortcut?: string;
    trayShortcutSettings?: TrayShortcutSettings;
    editorMode?: EditorMode;
    protectSensitiveWindows?: boolean;
    dataStoragePath?: string;
    dataStorageLabel?: string;
    busy?: boolean;
    error?: string | null;
    onsave?: (
      shortcut: string,
      trayShortcutSettings: TrayShortcutSettings,
    ) => void | Promise<void>;
    oncancel?: () => void;
    oneditormodechange?: (mode: EditorMode) => void | Promise<void>;
    onprotectsensitivechange?: (enabled: boolean) => void | Promise<void>;
    ondatastoragechange?: () => void | Promise<void>;
    onaboutopen?: () => void;
  }

  let {
    open = false,
    shortcut = "F1",
    trayShortcutSettings = { ...DEFAULT_TRAY_SHORTCUT_SETTINGS },
    editorMode = "typora",
    protectSensitiveWindows = false,
    dataStoragePath = "",
    dataStorageLabel = "尚未获取到存储路径",
    busy = false,
    error = null,
    onsave,
    oncancel,
    oneditormodechange,
    onprotectsensitivechange,
    ondatastoragechange,
    onaboutopen,
  }: Props = $props();

  let draft = $state("F1");
  let recording = $state(false);
  let saving = $state(false);
  let lastShortcut = $state("");
  let lastTraySettings = $state("");
  let trayDraft = $state<TrayShortcutSettings>({ ...DEFAULT_TRAY_SHORTCUT_SETTINGS });
  let submitting = $derived(busy || saving);
  let storagePathText = $derived(dataStoragePath || dataStorageLabel);

  const trayActionOptions: ReadonlyArray<{ value: TrayShortcutAction; label: string }> = [
    { value: "firstNote", label: "首个便签" },
    { value: "recentNote", label: "最近便签" },
    { value: "mainWindow", label: "主界面" },
    { value: "timer", label: "计时器" },
    { value: "reminder", label: "提醒" },
    { value: "gantt", label: "任务甘特图" },
    { value: "mfa", label: "MFA 验证器" },
    { value: "screenshot", label: "截图" },
  ];

  $effect(() => {
    if (open && shortcut !== lastShortcut) {
      lastShortcut = shortcut;
      draft = shortcut || "F1";
    }
    const traySignature = JSON.stringify(trayShortcutSettings);
    if (open && traySignature !== lastTraySettings) {
      lastTraySettings = traySignature;
      trayDraft = { ...trayShortcutSettings };
    }
    if (!open) {
      recording = false;
      lastShortcut = "";
      lastTraySettings = "";
    }
  });

  function normalizeKey(event: KeyboardEvent): string | null {
    if (["Control", "Shift", "Alt", "Meta"].includes(event.key)) return null;
    const key = event.key === " "
      ? "Space"
      : event.key.length === 1
        ? event.key.toUpperCase()
        : event.key;
    const modifiers: string[] = [];
    if (event.ctrlKey) modifiers.push("Ctrl");
    if (event.altKey) modifiers.push("Alt");
    if (event.shiftKey) modifiers.push("Shift");
    if (event.metaKey) modifiers.push("Super");
    const standalone = /^F(?:[1-9]|1\d|2[0-4])$/.test(key) || key === "PrintScreen";
    if (!standalone && modifiers.length === 0) return null;
    return [...modifiers, key].join("+");
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (!open) return;
    if (recording) {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        recording = false;
        return;
      }
      const next = normalizeKey(event);
      if (next) {
        draft = next;
        recording = false;
      }
      return;
    }
    if (event.key === "Escape" && !submitting) {
      event.preventDefault();
      oncancel?.();
    }
  }

  async function save(): Promise<void> {
    if (submitting || !draft) return;
    saving = true;
    try {
      await onsave?.(draft, { ...trayDraft });
    } finally {
      saving = false;
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open}
  <div class="settings-backdrop">
    <button
      type="button"
      class="backdrop-dismiss"
      aria-label="关闭设置"
      disabled={submitting}
      onclick={oncancel}
    ></button>
    <div class="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
      <header>
        <div>
          <h2 id="settings-title">设置</h2>
          <p>飞花 - PetalDesk 偏好设置</p>
        </div>
        <button type="button" class="icon-button" aria-label="关闭" disabled={submitting} onclick={oncancel}>
          <X size={18} aria-hidden="true" />
        </button>
      </header>

      <div class="settings-content">
        <section class="settings-section" aria-labelledby="general-settings-title">
          <h3 id="general-settings-title">常规</h3>
          <div class="setting-row editor-row">
            <div class="setting-copy">
              <strong>默认编辑样式</strong>
              <span>用于之后新建的便签</span>
            </div>
            <div class="mode-selector" role="group" aria-label="默认编辑样式">
              <button
                type="button"
                class:active={editorMode === "typora"}
                aria-pressed={editorMode === "typora"}
                disabled={submitting}
                onclick={() => oneditormodechange?.("typora")}
              >
                <FileText size={15} aria-hidden="true" />
                Markdown
              </button>
              <button
                type="button"
                class:active={editorMode === "plain"}
                aria-pressed={editorMode === "plain"}
                disabled={submitting}
                onclick={() => oneditormodechange?.("plain")}
              >
                纯文本
              </button>
            </div>
          </div>
        </section>

        <section class="settings-section" aria-labelledby="privacy-settings-title">
          <h3 id="privacy-settings-title">隐私与安全</h3>
          <div class="setting-row protect-row">
            <div class="setting-copy">
              <strong>保护敏感窗口</strong>
              <span>开启后，远程桌面会话中不会打开 MFA 验证器和密码管理器，窗口内容也会对截图、录屏和屏幕共享隐藏</span>
            </div>
            <div class="mode-selector" role="group" aria-label="保护敏感窗口">
              <button
                type="button"
                class:active={!protectSensitiveWindows}
                aria-pressed={!protectSensitiveWindows}
                disabled={submitting}
                onclick={() => onprotectsensitivechange?.(false)}
              >
                关闭
              </button>
              <button
                type="button"
                class:active={protectSensitiveWindows}
                aria-pressed={protectSensitiveWindows}
                disabled={submitting}
                onclick={() => onprotectsensitivechange?.(true)}
              >
                开启
              </button>
            </div>
          </div>
        </section>

        <section class="settings-section" aria-labelledby="storage-settings-title">
          <h3 id="storage-settings-title">飞花 - PetalDesk 数据存储</h3>
          <div class="storage-row">
            <div class="storage-path" title={storagePathText}>
              <FolderOpen size={17} aria-hidden="true" />
              <span>{storagePathText}</span>
            </div>
            <button
              type="button"
              class="change-path-button"
              disabled={submitting}
              onclick={ondatastoragechange}
            >更改</button>
          </div>
        </section>

        <section class="settings-section" aria-labelledby="tray-settings-title">
          <h3 id="tray-settings-title">托盘双击动作</h3>
          <p class="section-description">“双击”动作也用于桌面快捷方式启动或再次打开飞花</p>
          <div class="tray-shortcut-grid">
            <label>
              <span>双击</span>
              <select aria-label="双击打开" disabled={submitting} bind:value={trayDraft.doubleClick}>
                {#each trayActionOptions as option}
                  <option value={option.value}>{option.label}</option>
                {/each}
              </select>
            </label>
            <label>
              <span>{formatShortcut("Alt")} + 双击</span>
              <select aria-label="Alt 加双击打开" disabled={submitting} bind:value={trayDraft.altDoubleClick}>
                {#each trayActionOptions as option}
                  <option value={option.value}>{option.label}</option>
                {/each}
              </select>
            </label>
            <label>
              <span>{formatShortcut("Ctrl")} + 双击</span>
              <select aria-label="Ctrl 加双击打开" disabled={submitting} bind:value={trayDraft.ctrlDoubleClick}>
                {#each trayActionOptions as option}
                  <option value={option.value}>{option.label}</option>
                {/each}
              </select>
            </label>
            <label>
              <span>{formatShortcut("Shift")} + 双击</span>
              <select aria-label="Shift 加双击打开" disabled={submitting} bind:value={trayDraft.shiftDoubleClick}>
                {#each trayActionOptions as option}
                  <option value={option.value}>{option.label}</option>
                {/each}
              </select>
            </label>
          </div>
          <button
            type="button"
            class="reset-button tray-reset-button"
            disabled={submitting}
            onclick={() => (trayDraft = { ...DEFAULT_TRAY_SHORTCUT_SETTINGS })}
          >
            <RotateCcw size={15} aria-hidden="true" />
            恢复默认动作
          </button>
        </section>

        <section class="settings-section" aria-labelledby="screenshot-settings-title">
          <h3 id="screenshot-settings-title">截图</h3>
          <div class="setting-row shortcut-row">
            <div class="setting-copy">
              <strong>全局截图快捷键</strong>
              <span>飞花 - PetalDesk 在后台运行时也可以使用</span>
            </div>
            <button
              type="button"
              class:recording
              class="shortcut-recorder"
              aria-label="录入截图快捷键"
              aria-pressed={recording}
              disabled={submitting}
              onclick={() => (recording = true)}
            >
              <Keyboard size={16} aria-hidden="true" />
              <kbd>{recording ? "请按快捷键…" : formatShortcut(draft)}</kbd>
            </button>
          </div>

          <button
            type="button"
            class="reset-button"
            disabled={submitting || draft === "F1"}
            onclick={() => {
              draft = "F1";
              recording = false;
            }}
          >
            <RotateCcw size={15} aria-hidden="true" />
            恢复默认 F1
          </button>
        </section>

        <section class="settings-section" aria-labelledby="about-settings-title">
          <h3 id="about-settings-title">关于</h3>
          <div class="setting-row about-row">
            <div class="setting-copy">
              <strong>关于与更新</strong>
              <span>查看当前版本、自动更新设置和更新进度</span>
            </div>
            <button
              type="button"
              class="open-about-button"
              disabled={submitting}
              onclick={onaboutopen}
            >
              <Info size={15} aria-hidden="true" />
              打开关于与更新
            </button>
          </div>
        </section>
      </div>

      {#if error}
        <p class="error" role="alert">{error}</p>
      {/if}

      <footer>
        <button type="button" class="secondary-button" disabled={submitting} onclick={oncancel}>取消</button>
        <button type="button" class="primary-button" disabled={submitting || !draft} onclick={() => void save()}>
          {submitting ? "保存中…" : "保存"}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .settings-backdrop {
    position: fixed;
    z-index: 1000;
    inset: 0;
    display: grid;
    padding: 18px;
    place-items: center;
    background: rgb(0 0 0 / 28%);
  }

  .backdrop-dismiss {
    position: absolute;
    inset: 0;
    padding: 0;
    background: transparent;
    border: 0;
  }

  .settings-dialog {
    position: relative;
    width: min(100%, 520px);
    max-height: min(680px, calc(100vh - 36px));
    padding: 18px 18px 16px;
    overflow: auto;
    color: var(--app-fg);
    background: var(--app-surface);
    border: 1px solid var(--app-border);
    border-radius: 7px;
    box-shadow: var(--shadow-flyout);
  }

  header,
  footer,
  .setting-row,
  .storage-row,
  .storage-path,
  .mode-selector,
  .shortcut-recorder,
  .reset-button,
  .open-about-button {
    display: flex;
    align-items: center;
  }

  .section-description {
    margin: -5px 0 11px;
    color: var(--app-muted);
    font-size: 12px;
    line-height: 1.4;
  }

  .tray-shortcut-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px 12px;
  }

  .tray-shortcut-grid label {
    display: grid;
    grid-template-columns: minmax(88px, auto) minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .tray-shortcut-grid label > span {
    color: var(--app-fg);
    font-size: 12.5px;
    font-weight: 600;
    white-space: nowrap;
  }

  .tray-shortcut-grid select {
    width: 100%;
    min-width: 0;
    min-height: 32px;
    padding: 4px 24px 4px 8px;
    color: var(--app-fg);
    font: inherit;
    font-size: 12.5px;
    background: #fff;
    border: 1px solid var(--app-border-strong);
    border-radius: 4px;
  }

  .tray-shortcut-grid select:focus-visible {
    border-color: var(--app-accent);
    outline: 2px solid rgb(0 103 192 / 15%);
    outline-offset: 1px;
  }

  .tray-reset-button {
    margin-top: 8px;
  }

  header {
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }

  h2,
  p {
    margin: 0;
  }

  h2 {
    font-size: 17px;
    line-height: 1.35;
  }

  header p {
    margin-top: 2px;
    color: var(--app-muted);
    font-size: 12.5px;
  }

  .settings-content {
    display: grid;
    margin-top: 16px;
  }

  .settings-section {
    padding: 14px 0 16px;
    border-top: 1px solid var(--app-border);
  }

  .settings-section:last-child {
    padding-bottom: 2px;
  }

  h3 {
    margin: 0 0 12px;
    color: var(--app-muted);
    font-size: 11.5px;
    font-weight: 700;
    letter-spacing: 0;
  }

  .setting-row {
    min-width: 0;
    justify-content: space-between;
    gap: 18px;
  }

  .setting-copy {
    display: grid;
    min-width: 0;
    gap: 4px;
  }

  .setting-copy strong {
    font-size: 13.5px;
  }

  .setting-copy span {
    color: var(--app-muted);
    font-size: 12px;
    line-height: 1.4;
  }

  .mode-selector {
    flex: 0 0 auto;
  }

  .mode-selector button {
    display: inline-flex;
    min-width: 88px;
    min-height: 32px;
    padding: 5px 10px;
    align-items: center;
    justify-content: center;
    gap: 6px;
    color: var(--app-fg);
    font-size: 12.5px;
    font-weight: 600;
    background: #fff;
    border: 1px solid var(--app-border-strong);
  }

  .mode-selector button:first-child {
    border-radius: 4px 0 0 4px;
  }

  .mode-selector button:last-child {
    margin-left: -1px;
    border-radius: 0 4px 4px 0;
  }

  .mode-selector button.active {
    z-index: 1;
    color: #fff;
    background: var(--app-accent);
    border-color: #00589f;
  }

  .storage-row {
    min-width: 0;
    align-items: stretch;
    gap: 8px;
  }

  .storage-path {
    min-width: 0;
    min-height: 36px;
    flex: 1 1 auto;
    align-items: flex-start;
    gap: 8px;
    padding: 8px 10px;
    color: var(--app-muted);
    font-size: 12px;
    line-height: 1.45;
    background: #f7f7f7;
    border: 1px solid var(--app-border);
    border-radius: 4px;
  }

  .storage-path :global(svg) {
    flex: 0 0 auto;
    margin-top: 1px;
    color: var(--app-accent);
  }

  .storage-path span {
    min-width: 0;
    overflow-wrap: anywhere;
    user-select: text;
  }

  .change-path-button {
    min-width: 62px;
    min-height: 36px;
    padding: 5px 12px;
    align-self: stretch;
    color: var(--app-fg);
    font-size: 12.5px;
    font-weight: 600;
    background: #fff;
    border: 1px solid var(--app-border-strong);
    border-radius: 4px;
  }

  .change-path-button:hover:not(:disabled),
  .mode-selector button:hover:not(:disabled):not(.active),
  .open-about-button:hover:not(:disabled) {
    background: var(--app-surface-hover);
  }

  .shortcut-recorder {
    min-width: 138px;
    min-height: 34px;
    padding: 5px 10px;
    justify-content: center;
    gap: 8px;
    background: #fff;
    border: 1px solid var(--app-border-strong);
    border-radius: 4px;
  }

  .shortcut-recorder.recording {
    color: var(--app-accent);
    background: rgb(0 103 192 / 7%);
    border-color: var(--app-accent);
  }

  kbd {
    font-family: inherit;
    font-size: 12.5px;
    font-weight: 650;
    white-space: nowrap;
  }

  .reset-button {
    min-height: 30px;
    margin-top: 12px;
    padding: 4px 7px;
    gap: 6px;
    color: var(--app-accent);
    font-size: 12px;
    background: transparent;
    border: 0;
    border-radius: 4px;
  }

  .reset-button:hover:not(:disabled) {
    background: rgb(0 103 192 / 8%);
  }

  .reset-button:disabled {
    opacity: 0.48;
  }

  .open-about-button {
    min-height: 34px;
    padding: 5px 10px;
    flex: 0 0 auto;
    justify-content: center;
    gap: 6px;
    color: var(--app-accent);
    font-size: 12px;
    font-weight: 650;
    background: #fff;
    border: 1px solid var(--app-border-strong);
    border-radius: 4px;
  }

  .error {
    margin-top: 13px;
    padding: 8px 10px;
    color: #8f1d14;
    font-size: 12.5px;
    line-height: 1.4;
    background: #fff0ee;
    border-left: 3px solid var(--app-danger);
  }

  footer {
    margin-top: 18px;
    padding-top: 14px;
    justify-content: flex-end;
    gap: 8px;
    border-top: 1px solid var(--app-border);
  }

  .secondary-button,
  .primary-button {
    min-height: 32px;
    padding: 5px 12px;
    font-size: 12.5px;
    font-weight: 600;
    border-radius: 4px;
  }

  .secondary-button {
    background: #fff;
    border: 1px solid var(--app-border-strong);
  }

  .primary-button {
    color: #fff;
    background: var(--app-accent);
    border: 1px solid #00589f;
  }

  button:disabled {
    cursor: not-allowed;
    opacity: 0.62;
  }

  @media (max-width: 480px) {
    .setting-row,
    .storage-row {
      align-items: stretch;
      flex-direction: column;
    }

    .mode-selector,
    .shortcut-recorder,
    .change-path-button,
    .open-about-button {
      width: 100%;
    }

    .mode-selector button {
      width: 50%;
    }

    .tray-shortcut-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
