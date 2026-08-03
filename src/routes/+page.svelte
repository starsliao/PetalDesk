<script lang="ts">
  import { onMount, type Component } from "svelte";
  import { AlertCircle, FolderOpen, LoaderCircle } from "@lucide/svelte";
  import "$lib/styles/app.css";
  import { NoteShell, NotesList, TrashView } from "$lib/components";
  import AboutDialog from "$lib/components/AboutDialog.svelte";
  import ConfirmDialog from "$lib/components/ConfirmDialog.svelte";
  import ScreenshotSettingsDialog from "$lib/components/ScreenshotSettingsDialog.svelte";
  import LongCaptureControl from "$lib/screenshot/LongCaptureControl.svelte";
  import ReminderTool from "$lib/components/ReminderTool.svelte";
  import TimerTool from "$lib/components/TimerTool.svelte";
  import {
    DEFAULT_TRAY_SHORTCUT_SETTINGS,
    defaultEditorModeStorageKey,
    notesApi,
    type AppError,
    type AppInfo,
    type NoteColor,
    type NoteListItem,
    type NoteMeta,
    type NoteSnapshot,
    type TrayShortcutSettings,
    type TrashItem,
  } from "$lib/bridge";
  import { extractLocalImagePaths, type EditorMode } from "$lib/editor";
  import { screenshotApi, type ScreenshotSettings } from "$lib/screenshot";
  import { parseToolName, type ToolName } from "$lib/tools";
  import {
    prepareCurrentWindowForUpdate,
    updaterApi,
    type UpdateInstallPreparation,
  } from "$lib/updater";

  type NoteEditorComponent = typeof import("$lib/components/NoteEditor.svelte").default;
  type GanttToolComponent = typeof import("$lib/components/GanttTool.svelte").default;
  type MfaToolComponent = typeof import("$lib/components/MfaTool.svelte").default;
  type PasswordManagerToolComponent = Component<Record<string, never>>;
  type ScreenshotToolComponent = typeof import("$lib/components/ScreenshotTool.svelte").default;
  type PinnedScreenshotComponent = typeof import("$lib/components/PinnedScreenshot.svelte").default;

  type MainView = "notes" | "trash";
  type PendingNoteDelete = {
    id: string;
    title: string;
    source: "list" | "active";
  };
  type LongCaptureOutlineRect = {
    left: number;
    top: number;
    width: number;
    height: number;
  };

  const defaultLongCaptureOutlineRect: LongCaptureOutlineRect = {
    left: 0,
    top: 0,
    width: 100,
    height: 100,
  };

  function outlinePercent(search: URLSearchParams | null, name: string, fallback: number): number {
    const raw = search?.get(name);
    if (raw === null || raw === undefined || raw.trim() === "") return fallback;
    const value = Number(raw);
    if (!Number.isFinite(value)) return fallback;
    return Math.min(100, Math.max(0, value));
  }

  function parseLongCaptureOutlineRect(search: URLSearchParams | null): LongCaptureOutlineRect {
    const left = outlinePercent(search, "outlineLeft", defaultLongCaptureOutlineRect.left);
    const top = outlinePercent(search, "outlineTop", defaultLongCaptureOutlineRect.top);
    const width = Math.min(
      outlinePercent(search, "outlineWidth", defaultLongCaptureOutlineRect.width),
      100 - left,
    );
    const height = Math.min(
      outlinePercent(search, "outlineHeight", defaultLongCaptureOutlineRect.height),
      100 - top,
    );
    return { left, top, width, height };
  }

  function longCaptureOutlineMaskPath(rect: LongCaptureOutlineRect): string {
    const right = rect.left + rect.width;
    const bottom = rect.top + rect.height;
    return `M0 0H100V100H0Z M${rect.left} ${rect.top}V${bottom}H${right}V${rect.top}Z`;
  }

  const routeSearch =
    typeof window !== "undefined" ? new URLSearchParams(window.location.search) : null;
  const screenshotPinId = routeSearch?.get("screenshotPin") ?? null;
  const longCaptureControlId = routeSearch?.get("longControl") ?? null;
  const longCaptureOutlineId = routeSearch?.get("longOutline") ?? null;
  const longCaptureOutlineRect = parseLongCaptureOutlineRect(routeSearch);
  const longCaptureOutlinePath = longCaptureOutlineMaskPath(longCaptureOutlineRect);
  const longCaptureOutlineStyle = [
    `--outline-left:${longCaptureOutlineRect.left}%`,
    `--outline-top:${longCaptureOutlineRect.top}%`,
    `--outline-width:${longCaptureOutlineRect.width}%`,
    `--outline-height:${longCaptureOutlineRect.height}%`,
  ].join(";");
  const requestedTool = routeSearch?.get("tool") ?? null;
  const toolName: ToolName | null = parseToolName(requestedTool);
  const isToolWindow =
    toolName !== null || screenshotPinId !== null || longCaptureOutlineId !== null;

  let initialized = $state(false);
  let GanttTool = $state<GanttToolComponent | null>(null);
  let MfaTool = $state<MfaToolComponent | null>(null);
  let PasswordManagerTool = $state<PasswordManagerToolComponent | null>(null);
  let ScreenshotTool = $state<ScreenshotToolComponent | null>(null);
  let PinnedScreenshot = $state<PinnedScreenshotComponent | null>(null);
  let fatalError = $state("");
  let appInfo = $state<AppInfo | null>(null);
  let noteId = $state<string | null>(null);
  let mainView = $state<MainView>("notes");
  let notes = $state<NoteListItem[]>([]);
  let noteSwitcherNotes = $state<NoteListItem[]>([]);
  let noteSwitcherLoading = $state(false);
  let trash = $state<TrashItem[]>([]);
  let listLoading = $state(false);
  let reorderingNotes = $state(false);
  let query = $state("");
  let selectedId = $state<string | null>(null);
  let toast = $state("");
  let pendingNoteDelete = $state<PendingNoteDelete | null>(null);
  let deletingNote = $state(false);
  let pendingRestartPath = $state<string | null>(null);
  let restarting = $state(false);
  let settingsOpen = $state(false);
  let aboutOpen = $state(false);
  let settingsBusy = $state(false);
  let settingsError = $state<string | null>(null);
  let screenshotShortcut = $state("F1");
  let trayShortcutSettings = $state<TrayShortcutSettings>({
    ...DEFAULT_TRAY_SHORTCUT_SETTINGS,
  });

  let activeNote = $state<NoteSnapshot | null>(null);
  let NoteEditor = $state<NoteEditorComponent | null>(null);
  let markdownValue = $state("");
  let noteTitle = $state("无标题便签");
  let noteEditorMode = $state<EditorMode>("typora");
  let defaultEditorMode = $state<EditorMode>("typora");
  let assetUrls = $state<Record<string, string>>({});
  let noteLoading = $state(false);
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let bodyDirty = false;
  let pendingMeta: Partial<Pick<NoteMeta, "title" | "editorMode" | "color" | "pinned" | "readOnly">> = {};
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let windowStateTimer: ReturnType<typeof setTimeout> | undefined;
  let externalPollTimer: ReturnType<typeof setInterval> | undefined;
  let saveInFlight: Promise<void> | null = null;
  let reorderInFlight: Promise<void> | null = null;
  let queuedNoteOrder: string[] | null = null;
  const desktopCleanups: Array<() => void> = [];

  let activeTitle = $derived(noteTitle);

  function errorMessage(error: unknown): string {
    if (typeof error === "object" && error && "message" in error) {
      return String((error as AppError).message);
    }
    return typeof error === "string" ? error : "操作失败，请稍后重试。";
  }

  function showToast(message: string): void {
    toast = message;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ""), 2600);
  }

  async function initialize(): Promise<void> {
    try {
      noteId = new URLSearchParams(window.location.search).get("note");
      if (!noteId) {
        try {
          await notesApi.migrateLegacyTimerData();
        } catch {
          // Timer migration is best-effort and must not prevent notes from opening.
        }
      }
      appInfo = await notesApi.appInfo();
      defaultEditorMode = appInfo.defaultEditorMode ?? "typora";
      trayShortcutSettings = {
        ...(appInfo.trayShortcutSettings ?? DEFAULT_TRAY_SHORTCUT_SETTINGS),
      };
      if (notesApi.isDesktop()) {
        try {
          screenshotShortcut = (await screenshotApi.getSettings()).shortcut;
        } catch {
          // Keep the default label available if screenshot settings cannot be read.
        }
      }
      if (noteId) {
        NoteEditor = (await import("$lib/components/NoteEditor.svelte")).default;
        await loadActiveNote(noteId);
      }
      else await refreshMain();
      await registerDesktopEvents();
    } catch (error) {
      fatalError = errorMessage(error);
    } finally {
      initialized = true;
    }
  }

  async function refreshMain(): Promise<void> {
    listLoading = true;
    try {
      const [items, deleted] = await Promise.all([notesApi.listNotes(query), notesApi.listTrash()]);
      notes = items;
      trash = deleted;
    } finally {
      listLoading = false;
    }
  }

  async function refreshNoteSwitcher(): Promise<void> {
    if (!noteId || noteSwitcherLoading) return;
    noteSwitcherLoading = true;
    try {
      noteSwitcherNotes = await notesApi.listNotes();
    } catch (error) {
      showToast(errorMessage(error));
    } finally {
      noteSwitcherLoading = false;
    }
  }

  async function loadActiveNote(id: string): Promise<void> {
    noteLoading = true;
    saveError = null;
    try {
      const snapshot = await notesApi.getNote(id);
      activeNote = snapshot;
      markdownValue = snapshot.markdown;
      noteTitle = snapshot.meta.title;
      noteEditorMode = snapshot.meta.editorMode;
      bodyDirty = false;
      pendingMeta = {};
      await hydrateAssetUrls(snapshot.markdown);
      await updateWindowAppearance(snapshot.meta.pinned, snapshot.meta.title);
    } finally {
      noteLoading = false;
    }
  }

  async function hydrateAssetUrls(markdown: string): Promise<void> {
    if (!noteId) return;
    const paths = extractLocalImagePaths(markdown);
    const next: Record<string, string> = {};
    await Promise.all(
      [...new Set(paths)].map(async (path) => {
        try {
          next[path] = await notesApi.readAsset(noteId!, path);
        } catch {
          // Keep a broken image marker in the editor when an asset is missing.
        }
      }),
    );
    assetUrls = next;
  }

  async function registerDesktopEvents(): Promise<void> {
    if (notesApi.isDesktop()) {
      const [{ listen }, { getCurrentWindow }] = await Promise.all([
        import("@tauri-apps/api/event"),
        import("@tauri-apps/api/window"),
      ]);
      desktopCleanups.push(
        await listen<{ id?: string; noteId?: string; revision?: number; kind?: string }>(
          "note_changed",
          ({ payload }) => {
            const changedId = payload.id ?? payload.noteId;
            if (!noteId) {
              if (!reorderingNotes) void refreshMain();
            } else if (changedId === noteId) {
              // A note has one editor window, so committed events here are our own save echo.
              if (payload.kind === "committed" || payload.kind === "created") return;
              if (!bodyDirty && !saving && Object.keys(pendingMeta).length === 0) void loadActiveNote(noteId);
              else saveError = "检测到外部修改，当前内容尚未覆盖。";
            }
          },
        ),
      );
      desktopCleanups.push(
        await listen<{ mode: EditorMode }>("default_editor_mode_changed", ({ payload }) => {
          defaultEditorMode = payload.mode;
          if (appInfo) appInfo = { ...appInfo, defaultEditorMode: payload.mode };
        }),
      );
      desktopCleanups.push(
        await listen<{ settings?: ScreenshotSettings }>("screenshot_settings_changed", ({ payload }) => {
          if (payload.settings?.shortcut) screenshotShortcut = payload.settings.shortcut;
        }),
      );
      desktopCleanups.push(
        await listen<{ shortcut?: string; message?: string }>("screenshot_shortcut_error", ({ payload }) => {
          const shortcut = payload.shortcut || screenshotShortcut;
          showToast(payload.message || `截图快捷键 ${shortcut} 注册失败，请在设置中重试`);
        }),
      );
      desktopCleanups.push(
        await listen("open_about_dialog", () => {
          settingsOpen = false;
          aboutOpen = true;
        }),
      );

      if (noteId) {
        const windowHandle = getCurrentWindow();
        const scheduleWindowState = () => {
          if (windowStateTimer) clearTimeout(windowStateTimer);
          windowStateTimer = setTimeout(() => void notesApi.saveWindowState(), 220);
        };
        desktopCleanups.push(await windowHandle.onMoved(scheduleWindowState));
        desktopCleanups.push(await windowHandle.onResized(scheduleWindowState));
      }
    }

    const handleStorage = (event: StorageEvent) => {
      if (event.key === defaultEditorModeStorageKey && isEditorMode(event.newValue)) {
        defaultEditorMode = event.newValue;
        if (appInfo) appInfo = { ...appInfo, defaultEditorMode: event.newValue };
      }
    };
    const handleBrowserModeChange = (event: Event) => {
      const mode = (event as CustomEvent<unknown>).detail;
      if (isEditorMode(mode)) {
        defaultEditorMode = mode;
        if (appInfo) appInfo = { ...appInfo, defaultEditorMode: mode };
      }
    };
    window.addEventListener("storage", handleStorage);
    window.addEventListener("petaldesk:default-editor-mode-changed", handleBrowserModeChange);
    desktopCleanups.push(() => window.removeEventListener("storage", handleStorage));
    desktopCleanups.push(() => window.removeEventListener("petaldesk:default-editor-mode-changed", handleBrowserModeChange));

    // The Rust side already watches the workspace and emits `note_changed`, so
    // this is only a safety net for a missed event. Polling it every few seconds
    // duplicated that scan once per open window.
    externalPollTimer = setInterval(() => void pollExternalChanges(), 60_000);
  }

  async function registerUpdaterPreparation(): Promise<() => void> {
    if (!updaterApi.isSupported()) return () => undefined;
    const [{ listen }, { getCurrentWindow }] = await Promise.all([
      import("@tauri-apps/api/event"),
      import("@tauri-apps/api/window"),
    ]);
    const windowLabel = getCurrentWindow().label;
    const unlisten = await listen<UpdateInstallPreparation>("updater_prepare_install", async ({ payload }) => {
      let ok = true;
      let error: string | undefined;
      try {
        await prepareCurrentWindowForUpdate();
        if (!isToolWindow) {
          await flushSave();
          if (saveError) throw new Error(saveError);
          await notesApi.saveWindowState();
        }
      } catch (reason) {
        ok = false;
        error = errorMessage(reason);
      }
      try {
        await updaterApi.acknowledgeInstall(payload.requestId, windowLabel, ok, error);
      } catch {
        // The backend may have cancelled or timed out while this window saved.
      }
    });
    try {
      await updaterApi.registerInstallWindow();
    } catch (error) {
      unlisten();
      throw error;
    }
    return () => {
      // Keep the listener alive until the backend no longer includes this window
      // in an installation preparation snapshot.
      void updaterApi.unregisterInstallWindow().finally(unlisten);
    };
  }

  function isEditorMode(value: unknown): value is EditorMode {
    return value === "typora" || value === "plain";
  }

  async function pollExternalChanges(): Promise<void> {
    if (document.hidden || listLoading || noteLoading || reorderingNotes) return;
    try {
      if (!noteId || !activeNote) {
        await refreshMain();
        return;
      }
      const latest = await notesApi.getNote(noteId);
      if (latest.revision === activeNote.revision) return;
      if (bodyDirty || saving || Object.keys(pendingMeta).length > 0) {
        saveError = "文件已在其他程序中修改，当前内容尚未覆盖。";
        return;
      }
      activeNote = latest;
      markdownValue = latest.markdown;
      noteTitle = latest.meta.title;
      noteEditorMode = latest.meta.editorMode;
      pendingMeta = {};
      await hydrateAssetUrls(latest.markdown);
      await updateWindowAppearance(latest.meta.pinned, latest.meta.title);
      showToast("已载入外部修改");
    } catch {
      // A disconnected workspace is surfaced by the next explicit read or save.
    }
  }

  function scheduleSearch(next: string): void {
    query = next;
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => void refreshMain(), 160);
  }

  // Each save writes the journal, the body and the metadata, every one of them
  // fsynced. At 450ms a normal typing pause triggered that several times per
  // sentence; 1.2s still feels instant and cuts the write rate roughly threefold.
  function scheduleSave(delay = 1200): void {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void saveNow(), delay);
  }

  function handleMarkdownChange(value: string): void {
    markdownValue = value;
    bodyDirty = true;
    saveError = null;
    scheduleSave();
  }

  async function changeDefaultEditorMode(mode: EditorMode): Promise<void> {
    if (mode === defaultEditorMode) return;
    const previous = defaultEditorMode;
    defaultEditorMode = mode;
    if (appInfo) appInfo = { ...appInfo, defaultEditorMode: mode };
    try {
      const savedMode = await notesApi.setDefaultEditorMode(mode);
      defaultEditorMode = savedMode;
      if (appInfo) appInfo = { ...appInfo, defaultEditorMode: savedMode };
    } catch (error) {
      defaultEditorMode = previous;
      if (appInfo) appInfo = { ...appInfo, defaultEditorMode: previous };
      showToast(errorMessage(error));
    }
  }

  function setNoteEditorMode(mode: EditorMode): void {
    if (!activeNote || mode === noteEditorMode) return;
    noteEditorMode = mode;
    activeNote = { ...activeNote, meta: { ...activeNote.meta, editorMode: mode } };
    pendingMeta = { ...pendingMeta, editorMode: mode };
    scheduleSave(80);
  }

  function setTitle(title: string): void {
    if (!activeNote || activeNote.meta.readOnly) return;
    const normalized = title.trim().slice(0, 200) || "无标题便签";
    if (normalized === noteTitle) return;
    noteTitle = normalized;
    activeNote = { ...activeNote, meta: { ...activeNote.meta, title: normalized } };
    pendingMeta = { ...pendingMeta, title: normalized };
    void updateWindowTitle(normalized);
    scheduleSave(80);
  }

  function setReadOnly(readOnly: boolean): void {
    if (!activeNote || readOnly === activeNote.meta.readOnly) return;
    activeNote = { ...activeNote, meta: { ...activeNote.meta, readOnly } };
    pendingMeta = { ...pendingMeta, readOnly };
    scheduleSave(80);
  }

  async function performSave(): Promise<void> {
    if (!activeNote || (!bodyDirty && Object.keys(pendingMeta).length === 0)) return;

    const markdown = markdownValue;
    const metaPatch = { ...pendingMeta };
    const baseRevision = activeNote.revision;
    const baseContentHash = activeNote.contentHash;
    const includedBody = bodyDirty;
    bodyDirty = false;
    pendingMeta = {};
    saving = true;
    saveError = null;

    try {
      const result = await notesApi.commitNote({
        id: activeNote.id,
        baseRevision,
        baseContentHash,
        markdown,
        metaPatch,
      });
      activeNote = {
        ...activeNote,
        markdown,
        revision: result.revision,
        contentHash: result.contentHash,
        meta: { ...activeNote.meta, ...metaPatch, updatedAt: result.savedAt },
      };
    } catch (error) {
      if (includedBody) bodyDirty = true;
      pendingMeta = { ...metaPatch, ...pendingMeta };
      saveError = errorMessage(error);
    } finally {
      saving = false;
    }

    if ((bodyDirty || Object.keys(pendingMeta).length > 0) && !saveError) scheduleSave(80);
  }

  async function saveNow(): Promise<void> {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = undefined;
    if (saveInFlight) {
      await saveInFlight;
      if (bodyDirty || Object.keys(pendingMeta).length > 0) await saveNow();
      return;
    }
    saveInFlight = performSave();
    try {
      await saveInFlight;
    } finally {
      saveInFlight = null;
    }
  }

  async function flushSave(): Promise<void> {
    if (saveError) return;
    await saveNow();
    if (!saveError && (bodyDirty || Object.keys(pendingMeta).length > 0)) await saveNow();
  }

  async function setColor(color: NoteColor): Promise<void> {
    if (!activeNote) return;
    activeNote = { ...activeNote, meta: { ...activeNote.meta, color } };
    pendingMeta = { ...pendingMeta, color };
    scheduleSave(80);
  }

  async function setPinned(pinned: boolean): Promise<void> {
    if (!activeNote) return;
    activeNote = { ...activeNote, meta: { ...activeNote.meta, pinned } };
    pendingMeta = { ...pendingMeta, pinned };
    await updateWindowAppearance(pinned, activeTitle);
    scheduleSave(80);
  }

  async function updateWindowAppearance(pinned: boolean, title: string): Promise<void> {
    if (!notesApi.isDesktop()) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const windowHandle = getCurrentWindow();
    await Promise.all([windowHandle.setAlwaysOnTop(pinned), windowHandle.setTitle(`${title} - 飞花 - PetalDesk`)]);
  }

  async function updateWindowTitle(title: string): Promise<void> {
    if (!notesApi.isDesktop()) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setTitle(`${title} - 飞花 - PetalDesk`);
  }

  async function importAsset(file: File): Promise<string> {
    if (!activeNote) throw new Error("便签尚未加载完成。");
    const asset = await notesApi.importAsset(activeNote.id, file);
    try {
      const url = await notesApi.readAsset(activeNote.id, asset.relativePath);
      assetUrls = { ...assetUrls, [asset.relativePath]: url };
    } catch {
      if (asset.relativePath.startsWith("blob:")) {
        assetUrls = { ...assetUrls, [asset.relativePath]: asset.relativePath };
      }
    }
    return asset.relativePath;
  }

  async function openExternalLink(url: string): Promise<void> {
    try {
      await notesApi.openExternalLink(url);
    } catch (error) {
      showToast(errorMessage(error));
    }
  }

  async function createNote(): Promise<void> {
    try {
      const created = await notesApi.createNote();
      await notesApi.openNoteWindow(created.id);
      if (!noteId) await refreshMain();
    } catch (error) {
      showToast(errorMessage(error));
    }
  }

  async function openNote(id: string): Promise<void> {
    try {
      await notesApi.openNoteWindow(id);
    } catch (error) {
      showToast(errorMessage(error));
    }
  }

  async function openTool(tool: ToolName): Promise<void> {
    try {
      await notesApi.openToolWindow(tool);
    } catch (error) {
      showToast(errorMessage(error));
    }
  }

  async function openSettings(): Promise<void> {
    settingsOpen = true;
    settingsBusy = true;
    settingsError = null;
    try {
      const latestAppInfo = await notesApi.appInfo();
      appInfo = latestAppInfo;
      defaultEditorMode = latestAppInfo.defaultEditorMode ?? "typora";
      trayShortcutSettings = {
        ...(latestAppInfo.trayShortcutSettings ?? DEFAULT_TRAY_SHORTCUT_SETTINGS),
      };
      screenshotShortcut = (await screenshotApi.getSettings()).shortcut;
    } catch (error) {
      settingsError = errorMessage(error);
    } finally {
      settingsBusy = false;
    }
  }

  async function saveSettings(
    shortcut: string,
    nextTrayShortcutSettings: TrayShortcutSettings,
  ): Promise<void> {
    settingsBusy = true;
    settingsError = null;
    try {
      const settings = await screenshotApi.setShortcut(shortcut);
      screenshotShortcut = settings.shortcut;
      const savedTrayShortcutSettings = await notesApi.setTrayShortcutSettings(
        nextTrayShortcutSettings,
      );
      trayShortcutSettings = { ...savedTrayShortcutSettings };
      if (appInfo) {
        appInfo = { ...appInfo, trayShortcutSettings: { ...savedTrayShortcutSettings } };
      }
      settingsOpen = false;
      showToast("设置已保存");
    } catch (error) {
      settingsError = errorMessage(error);
    } finally {
      settingsBusy = false;
    }
  }

  async function toggleListPin(id: string, pinned: boolean): Promise<void> {
    try {
      const snapshot = await notesApi.getNote(id);
      await notesApi.commitNote({
        id,
        baseRevision: snapshot.revision,
        baseContentHash: snapshot.contentHash,
        markdown: snapshot.markdown,
        metaPatch: { pinned },
      });
      await refreshMain();
    } catch (error) {
      showToast(errorMessage(error));
    }
  }

  function reorderMainNotes(orderedIds: string[]): void {
    if (query.trim() || orderedIds.length !== notes.length) return;
    const byId = new Map(notes.map((note) => [note.id, note]));
    const reordered = orderedIds.flatMap((id) => {
      const note = byId.get(id);
      return note ? [note] : [];
    });
    if (reordered.length !== notes.length) return;

    notes = reordered;
    queuedNoteOrder = orderedIds;
    if (reorderInFlight) return;

    reorderingNotes = true;
    reorderInFlight = persistQueuedNoteOrder();
  }

  async function persistQueuedNoteOrder(): Promise<void> {
    try {
      while (queuedNoteOrder) {
        const orderedIds = queuedNoteOrder;
        queuedNoteOrder = null;
        try {
          const saved = await notesApi.reorderNotes(orderedIds);
          if (!queuedNoteOrder) notes = saved;
        } catch (error) {
          queuedNoteOrder = null;
          try {
            await refreshMain();
          } catch {
            // Keep the original reorder error visible when refreshing also fails.
          }
          showToast(errorMessage(error));
          break;
        }
      }
    } finally {
      reorderingNotes = false;
      reorderInFlight = null;
    }
  }

  function requestDeleteFromList(id: string): void {
    const note = notes.find((item) => item.id === id);
    pendingNoteDelete = {
      id,
      title: note?.title || "无标题便签",
      source: "list",
    };
  }

  function requestDeleteActiveNote(): void {
    if (!activeNote) return;
    pendingNoteDelete = {
      id: activeNote.id,
      title: activeNote.meta.title || "无标题便签",
      source: "active",
    };
  }

  async function confirmDeleteNote(): Promise<void> {
    const request = pendingNoteDelete;
    if (!request || deletingNote) return;
    deletingNote = true;
    try {
      if (request.source === "active") {
        if (saveTimer) clearTimeout(saveTimer);
        bodyDirty = false;
        pendingMeta = {};
        await notesApi.deleteNote(request.id);
        pendingNoteDelete = null;
        await closeCurrentWindow(false);
      } else {
        await notesApi.deleteNote(request.id);
        if (selectedId === request.id) selectedId = null;
        pendingNoteDelete = null;
        await refreshMain();
        showToast("便签已移到回收站");
      }
    } catch (error) {
      showToast(errorMessage(error));
    } finally {
      deletingNote = false;
    }
  }

  async function restoreNote(id: string): Promise<void> {
    try {
      await notesApi.restoreNote(id);
      await refreshMain();
      showToast("便签已恢复");
    } catch (error) {
      showToast(errorMessage(error));
    }
  }

  async function emptyTrash(): Promise<void> {
    await notesApi.emptyTrash();
    await refreshMain();
    showToast("回收站已清空");
  }

  async function chooseDataStoragePath(): Promise<void> {
    try {
      const result = await notesApi.chooseDataStoragePath();
      if (!result) return;
      if (result.restartRequired) {
        settingsOpen = false;
        pendingRestartPath = result.path;
      } else {
        if (appInfo) appInfo = { ...appInfo, workspacePath: result.path };
        await refreshMain();
        showToast("飞花 - PetalDesk 数据存储路径已更新");
      }
    } catch (error) {
      showToast(errorMessage(error));
    }
  }

  async function restartAfterStorageChange(): Promise<void> {
    if (restarting) return;
    restarting = true;
    try {
      await notesApi.restartApp();
    } catch (error) {
      restarting = false;
      showToast(errorMessage(error));
    }
  }

  function postponeRestart(): void {
    if (restarting) return;
    pendingRestartPath = null;
    showToast("新路径将在下次启动“飞花 - PetalDesk”时生效");
  }

  async function closeCurrentWindow(flush = true): Promise<void> {
    if (flush) await flushSave();
    if (notesApi.isDesktop()) {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await notesApi.saveWindowState();
      if (noteId) await notesApi.closeNoteWindow(noteId);
      await getCurrentWindow().destroy();
    } else {
      window.close();
      if (!window.closed) {
        const url = new URL(window.location.href);
        url.search = "";
        window.location.assign(url);
      }
    }
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    if (isToolWindow) return;
    const modifier = event.ctrlKey || event.metaKey;
    if (!modifier) return;
    const key = event.key.toLocaleLowerCase();
    if (key === "n") {
      event.preventDefault();
      void createNote();
    } else if (event.shiftKey && key === "f") {
      event.preventDefault();
      if (!noteId) {
        mainView = "notes";
        requestAnimationFrame(() => document.querySelector<HTMLInputElement>('input[type="search"]')?.focus());
      }
    } else if (event.shiftKey && key === "m" && noteId) {
      event.preventDefault();
      setNoteEditorMode(noteEditorMode === "typora" ? "plain" : "typora");
    }
  }

  onMount(() => {
    let disposed = false;
    let updaterCleanup: (() => void) | undefined;
    void registerUpdaterPreparation()
      .then((cleanup) => {
        if (disposed) cleanup();
        else updaterCleanup = cleanup;
      })
      .catch(() => undefined);

    if (isToolWindow) {
      const timerPage = toolName === "timer";
      const transparentPage = timerPage || longCaptureOutlineId !== null;
      document.documentElement.classList.toggle("timer-tool-page", timerPage);
      document.body.classList.toggle("timer-tool-page", timerPage);
      document.documentElement.classList.toggle("transparent-tool-page", transparentPage);
      document.body.classList.toggle("transparent-tool-page", transparentPage);
      if (screenshotPinId) {
        void import("$lib/components/PinnedScreenshot.svelte").then((module) => {
          if (!disposed) PinnedScreenshot = module.default;
        });
      } else if (toolName === "screenshot" && !longCaptureControlId && !longCaptureOutlineId) {
        void import("$lib/components/ScreenshotTool.svelte").then((module) => {
          if (!disposed) ScreenshotTool = module.default;
        });
      } else if (toolName === "gantt") {
        void import("$lib/components/GanttTool.svelte").then((module) => {
          if (!disposed) GanttTool = module.default;
        });
      } else if (toolName === "mfa") {
        void import("$lib/components/MfaTool.svelte").then((module) => {
          if (!disposed) MfaTool = module.default;
        });
      } else if (toolName === "passwords") {
        void import("$lib/components/PasswordManagerTool.svelte").then((module) => {
          if (!disposed) PasswordManagerTool = module.default;
        });
      }
      return () => {
        disposed = true;
        updaterCleanup?.();
        document.documentElement.classList.remove("timer-tool-page");
        document.body.classList.remove("timer-tool-page");
        document.documentElement.classList.remove("transparent-tool-page");
        document.body.classList.remove("transparent-tool-page");
      };
    }

    void initialize();
    const beforeUnload = () => {
      void flushSave();
      void notesApi.saveWindowState();
    };
    const visibilityChange = () => {
      if (document.hidden) {
        void flushSave();
        void notesApi.saveWindowState();
      }
    };
    window.addEventListener("beforeunload", beforeUnload);
    document.addEventListener("visibilitychange", visibilityChange);
    return () => {
      disposed = true;
      updaterCleanup?.();
      if (saveTimer) clearTimeout(saveTimer);
      if (searchTimer) clearTimeout(searchTimer);
      if (toastTimer) clearTimeout(toastTimer);
      if (windowStateTimer) clearTimeout(windowStateTimer);
      if (externalPollTimer) clearInterval(externalPollTimer);
      desktopCleanups.splice(0).forEach((cleanup) => cleanup());
      window.removeEventListener("beforeunload", beforeUnload);
      document.removeEventListener("visibilitychange", visibilityChange);
    };
  });
</script>

<svelte:window onkeydown={handleGlobalKeydown} />

<svelte:head>
  <title>{screenshotPinId
      ? "贴图 - 飞花 - PetalDesk"
      : longCaptureOutlineId
        ? "长截图范围 - 飞花 - PetalDesk"
        : toolName === "timer"
        ? "计时器 - 飞花 - PetalDesk"
        : toolName === "reminder"
          ? "提醒 - 飞花 - PetalDesk"
          : toolName === "gantt"
            ? "任务甘特图 - 飞花 - PetalDesk"
            : toolName === "mfa"
              ? "MFA 验证器 - 飞花 - PetalDesk"
              : toolName === "passwords"
                ? "密码管理器 - 飞花 - PetalDesk"
              : toolName === "screenshot"
                ? longCaptureControlId
                  ? "长截图控制 - 飞花 - PetalDesk"
                  : "截图 - 飞花 - PetalDesk"
                : noteId
                  ? `${activeTitle} - 飞花 - PetalDesk`
                  : "飞花 - PetalDesk"}</title>
</svelte:head>

{#if screenshotPinId}
  <main class="pinned-screenshot-window">
    {#if PinnedScreenshot}
      <PinnedScreenshot pinId={screenshotPinId} />
    {:else}
      <div class="tool-loading transparent-loading" aria-busy="true">
        <LoaderCircle class="spinner" size={20} aria-hidden="true" />
      </div>
    {/if}
  </main>
{:else if longCaptureOutlineId}
  <main
    class="long-capture-outline-window"
    data-testid="long-capture-outline"
    data-outline-left={longCaptureOutlineRect.left}
    data-outline-top={longCaptureOutlineRect.top}
    data-outline-width={longCaptureOutlineRect.width}
    data-outline-height={longCaptureOutlineRect.height}
    style={longCaptureOutlineStyle}
    aria-hidden="true"
  >
    <svg class="long-capture-outline-mask" viewBox="0 0 100 100" preserveAspectRatio="none">
      <path d={longCaptureOutlinePath} fill-rule="evenodd"></path>
    </svg>
    <div class="long-capture-outline-border"></div>
  </main>
{:else if toolName === "screenshot" && longCaptureControlId}
  <main class="long-capture-control-window">
    <!-- Keep focus on the target app so wheel and keyboard input reach its scroll area. -->
    <LongCaptureControl jobId={longCaptureControlId} keyboardShortcuts={false} />
  </main>
{:else if toolName === "screenshot"}
  <main class="screenshot-tool-window">
    {#if ScreenshotTool}
      <ScreenshotTool />
    {:else}
      <div class="tool-loading screenshot-loading" aria-busy="true">
        <LoaderCircle class="spinner" size={20} aria-hidden="true" />
      </div>
    {/if}
  </main>
{:else if toolName === "timer"}
  <main class="timer-tool-window">
    <TimerTool />
  </main>
{:else if toolName === "reminder"}
  <main class="reminder-tool-window">
    <ReminderTool />
  </main>
{:else if toolName === "gantt"}
  <main class="gantt-tool-window">
    {#if GanttTool}
      <GanttTool />
    {:else}
      <div class="tool-loading" aria-busy="true">
        <LoaderCircle class="spinner" size={20} aria-hidden="true" />
      </div>
    {/if}
  </main>
{:else if toolName === "mfa"}
  <main class="mfa-tool-window">
    {#if MfaTool}
      <MfaTool />
    {:else}
      <div class="tool-loading" aria-busy="true">
        <LoaderCircle class="spinner" size={20} aria-hidden="true" />
      </div>
    {/if}
  </main>
{:else if toolName === "passwords"}
  <main class="password-tool-window">
    {#if PasswordManagerTool}
      <PasswordManagerTool />
    {:else}
      <div class="tool-loading" aria-busy="true">
        <LoaderCircle class="spinner" size={20} aria-hidden="true" />
      </div>
    {/if}
  </main>
{:else if !initialized}
  <main class="startup-state" aria-busy="true">
    <img src="/app-icon.svg" alt="" />
    <LoaderCircle class="spinner" size={22} aria-hidden="true" />
    <span>正在打开飞花 - PetalDesk…</span>
  </main>
{:else if fatalError}
  <main class="fatal-state" role="alert">
    <AlertCircle size={28} aria-hidden="true" />
    <h1>无法打开飞花 - PetalDesk</h1>
    <p>{fatalError}</p>
    <button type="button" onclick={() => window.location.reload()}>重新加载</button>
  </main>
{:else if noteId}
  {#if noteLoading || !activeNote || !NoteEditor}
    <main class="startup-state note-loading" aria-busy="true">
      <LoaderCircle class="spinner" size={20} aria-hidden="true" />
      <span>正在打开便签…</span>
    </main>
  {:else}
    <NoteShell
      title={activeTitle}
      color={activeNote.meta.color}
      pinned={activeNote.meta.pinned}
      editorMode={noteEditorMode}
      {screenshotShortcut}
      notes={noteSwitcherNotes}
      currentNoteId={activeNote.id}
      notesLoading={noteSwitcherLoading}
      readonly={activeNote.meta.readOnly}
      {saving}
      {saveError}
      onnew={createNote}
      ontitlechange={setTitle}
      oncolorchange={setColor}
      oneditormodechange={setNoteEditorMode}
      onreadonlychange={setReadOnly}
      ontogglepin={setPinned}
      ontoolopen={openTool}
      onnotesopen={refreshNoteSwitcher}
      onnoteopen={openNote}
      ondelete={requestDeleteActiveNote}
      onclose={() => void closeCurrentWindow()}
    >
      <NoteEditor
        value={markdownValue}
        mode={noteEditorMode}
        readonly={activeNote.meta.readOnly}
        autofocus
        {assetUrls}
        onasset={importAsset}
        onchange={handleMarkdownChange}
        onopenlink={(url) => void openExternalLink(url)}
        onerror={(detail) => (saveError = errorMessage(detail.error))}
      />
    </NoteShell>
  {/if}
{:else}
  <main class="main-window">
    {#if mainView === "trash"}
      <TrashView
        notes={trash}
        loading={listLoading}
        onback={() => (mainView = "notes")}
        onrestore={restoreNote}
        onempty={emptyTrash}
      />
    {:else}
      <NotesList
        {notes}
        {selectedId}
        {query}
        loading={listLoading}
        reorderBusy={reorderingNotes}
        trashCount={trash.length}
        {screenshotShortcut}
        onquerychange={scheduleSearch}
        onreorder={reorderMainNotes}
        oncreate={createNote}
        onselect={(id) => (selectedId = id)}
        onopen={openNote}
        ontogglepin={toggleListPin}
        ondelete={requestDeleteFromList}
        onshowtrash={() => (mainView = "trash")}
        onsettingsopen={() => void openSettings()}
        ontoolopen={openTool}
      />
    {/if}
  </main>
{/if}

{#if !isToolWindow && settingsOpen}
  <ScreenshotSettingsDialog
    open
    shortcut={screenshotShortcut}
    {trayShortcutSettings}
    editorMode={defaultEditorMode}
    dataStoragePath={appInfo?.workspacePath ?? ""}
    busy={settingsBusy}
    error={settingsError}
    onsave={saveSettings}
    oneditormodechange={(mode) => void changeDefaultEditorMode(mode)}
    ondatastoragechange={() => void chooseDataStoragePath()}
    onaboutopen={() => {
      if (settingsBusy) return;
      settingsOpen = false;
      aboutOpen = true;
    }}
    oncancel={() => {
      if (!settingsBusy) settingsOpen = false;
    }}
  />
{/if}

{#if !isToolWindow && aboutOpen}
  <AboutDialog
    currentVersion={appInfo?.version ?? ""}
    onclose={() => (aboutOpen = false)}
  />
{/if}

{#if !isToolWindow && toast}
  <div class="toast" role="status">{toast}</div>
{/if}

{#if !isToolWindow && pendingNoteDelete}
  <ConfirmDialog
    open
    title="将便签移到回收站？"
    detail={`“${pendingNoteDelete.title}”将移到回收站，你可以稍后从回收站恢复。`}
    confirmLabel="移到回收站"
    busy={deletingNote}
    onconfirm={confirmDeleteNote}
    oncancel={() => {
      if (!deletingNote) pendingNoteDelete = null;
    }}
  />
{/if}

{#if !isToolWindow && pendingRestartPath}
  <ConfirmDialog
    open
    title="需要重启飞花 - PetalDesk"
    detail={`飞花 - PetalDesk 数据存储路径已修改为“${pendingRestartPath}”。重启后新路径才会生效。`}
    confirmLabel="立即重启"
    cancelLabel="稍后"
    tone="primary"
    busy={restarting}
    onconfirm={restartAfterStorageChange}
    oncancel={postponeRestart}
  />
{/if}

<style>
  :global(html),
  :global(body),
  :global(body > div) {
    width: 100%;
    height: 100%;
  }

  .main-window,
  .startup-state,
  .fatal-state {
    width: 100%;
    height: 100%;
  }

  .timer-tool-window {
    width: 100vw;
    height: 100vh;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: transparent;
  }

  .reminder-tool-window,
  .gantt-tool-window,
  .mfa-tool-window,
  .password-tool-window {
    width: 100vw;
    height: 100vh;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    color: var(--app-fg);
    background: var(--app-bg);
  }

  .screenshot-tool-window,
  .long-capture-control-window,
  .long-capture-outline-window,
  .pinned-screenshot-window {
    width: 100vw;
    height: 100vh;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  .screenshot-tool-window,
  .screenshot-loading {
    color: #ffffff;
    background: #111111;
  }

  .long-capture-control-window {
    width: 100vw;
    height: 100vh;
    overflow: hidden;
    color: #242424;
    background: #fafafa;
  }

  .long-capture-outline-window {
    position: fixed;
    inset: 0;
    box-sizing: border-box;
    width: 100vw;
    height: 100vh;
    background: transparent;
    pointer-events: none;
  }

  .long-capture-outline-mask {
    position: absolute;
    inset: 0;
    display: block;
    width: 100%;
    height: 100%;
    overflow: visible;
    pointer-events: none;
  }

  .long-capture-outline-mask path {
    fill: rgb(0 0 0 / 50%);
    pointer-events: none;
  }

  .long-capture-outline-border {
    position: absolute;
    box-sizing: border-box;
    top: var(--outline-top);
    left: var(--outline-left);
    width: var(--outline-width);
    height: var(--outline-height);
    background: transparent;
    outline: 2px solid #00a2ff;
    outline-offset: 0;
    box-shadow:
      0 0 0 1px rgb(0 0 0 / 78%),
      0 0 5px rgb(0 162 255 / 48%);
    pointer-events: none;
  }

  .pinned-screenshot-window,
  .transparent-loading {
    color: #ffffff;
    background: transparent;
  }

  .tool-loading {
    display: grid;
    width: 100%;
    height: 100%;
    color: var(--app-muted);
    place-items: center;
  }

  :global(html.transparent-tool-page),
  :global(body.transparent-tool-page) {
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: transparent !important;
  }

  .startup-state,
  .fatal-state {
    display: flex;
    padding: 24px;
    align-items: center;
    justify-content: center;
    flex-direction: column;
    color: var(--app-muted);
    text-align: center;
    background: var(--app-bg);
  }

  .startup-state img {
    width: 58px;
    height: 58px;
    margin-bottom: 22px;
  }

  .startup-state span {
    margin-top: 9px;
    font-size: 12.5px;
  }

  .note-loading {
    color: #665b31;
    background: #fff1a8;
  }

  :global(.spinner) {
    animation: spin 850ms linear infinite;
  }

  .fatal-state {
    color: var(--app-danger);
  }

  .fatal-state h1 {
    margin: 14px 0 0;
    color: var(--app-fg);
    font-size: 18px;
  }

  .fatal-state p {
    max-width: 420px;
    margin: 8px 0 18px;
    color: var(--app-muted);
    font-size: 13px;
    line-height: 1.5;
  }

  .fatal-state button {
    min-height: 32px;
    padding: 5px 14px;
    color: #ffffff;
    background: var(--app-accent);
    border: 1px solid #00589f;
    border-radius: 4px;
  }

  .toast {
    position: fixed;
    z-index: 2000;
    right: 16px;
    bottom: 16px;
    max-width: min(360px, calc(100vw - 32px));
    padding: 9px 12px;
    color: #ffffff;
    font-size: 12.5px;
    line-height: 1.4;
    background: #292929;
    border-radius: 5px;
    box-shadow: 0 6px 20px rgb(0 0 0 / 20%);
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
