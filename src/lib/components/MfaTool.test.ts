import { cleanup, fireEvent, render, waitFor, within } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createBrowserMfaApi,
  mfaApi,
  type MfaApi,
  type MfaEntrySummary,
  type MfaEntryUpdateRequest,
  type MfaImportPreview,
  type MfaManualImportRequest,
} from "../mfa";
import MfaTool from "./MfaTool.svelte";

const backendInvoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke: backendInvoke }));

interface TauriTestWindow extends Window {
  __TAURI_INTERNALS__?: object;
}

afterEach(() => {
  cleanup();
  backendInvoke.mockReset();
  delete (window as TauriTestWindow).__TAURI_INTERNALS__;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("mfaApi recovery commands", () => {
  it("sends recovery passwords using the backend command parameter name", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockResolvedValue(undefined);

    await mfaApi.configureRecoveryPassword("correct horse battery");
    await mfaApi.unlockWithRecoveryPassword("migration recovery code");

    expect(backendInvoke).toHaveBeenNthCalledWith(1, "configure_mfa_recovery_password", {
      password: "correct horse battery",
    });
    expect(backendInvoke).toHaveBeenNthCalledWith(2, "unlock_mfa_with_recovery_password", {
      password: "migration recovery code",
    });
  });

  it("uses the exact backend argument names for reorder and pin commands", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockResolvedValue([]);

    await mfaApi.reorder(["first", "second"]);
    await mfaApi.setPinned("second", true);

    expect(backendInvoke).toHaveBeenNthCalledWith(1, "reorder_mfa_entries", {
      orderedIds: ["first", "second"],
    });
    expect(backendInvoke).toHaveBeenNthCalledWith(2, "set_mfa_entry_pinned", {
      entryId: "second",
      pinned: true,
    });
  });

  it("keeps browser-demo pin and reorder operations in memory", async () => {
    const api = createBrowserMfaApi();
    const initial = await api.list();
    const github = initial.find((item) => item.name === "GitHub")!;
    const petaldesk = initial.find((item) => item.name === "飞花演示")!;

    await api.setPinned(github.id, true);
    expect((await api.list())[0]).toMatchObject({ id: github.id, pinned: true });
    await api.setPinned(github.id, false);
    await api.reorder([petaldesk.id, github.id]);

    const saved = await api.list();
    expect(saved.map((item) => item.id)).toEqual([petaldesk.id, github.id]);
    expect(saved.every((item) => !item.pinned)).toBe(true);
  });
});

function entry(overrides: Partial<MfaEntrySummary> = {}): MfaEntrySummary {
  return {
    id: "550e8400-e29b-41d4-a716-446655440000",
    name: "GitHub",
    issuer: "GitHub",
    accountName: "octocat",
    iconEmoji: "🐙",
    pinned: false,
    algorithm: "sha1",
    digits: 6,
    period: 30,
    createdAt: "2026-08-01T08:00:00Z",
    updatedAt: "2026-08-01T08:00:00Z",
    ...overrides,
  };
}

function preview(overrides: Partial<MfaImportPreview> = {}): MfaImportPreview {
  return {
    sessionId: "preview-session",
    name: "GitHub",
    issuer: "GitHub",
    accountName: "person@example.com",
    iconEmoji: "🐙",
    algorithm: "sha1",
    digits: 6,
    period: 30,
    warnings: [],
    ...overrides,
  };
}

function mockApi(initial: MfaEntrySummary[] = [entry()]): MfaApi & Record<string, ReturnType<typeof vi.fn> | (() => boolean)> {
  let items = structuredClone(initial);
  return {
    isDesktop: () => false,
    getStatus: vi.fn().mockResolvedValue({
      available: true,
      locked: false,
      entryCount: items.length,
      protection: "windows-dpapi-recovery-password",
      recoveryState: "ready",
      captureExcluded: true,
    }),
    list: vi.fn().mockImplementation(async () => structuredClone(items)),
    scanScreenQr: vi.fn().mockResolvedValue([preview()]),
    previewQrImage: vi.fn().mockResolvedValue([preview()]),
    previewUri: vi.fn().mockResolvedValue([preview()]),
    previewManual: vi.fn().mockResolvedValue([preview()]),
    commitImport: vi.fn().mockImplementation(async (_sessionId: string, iconEmoji: string) => {
      const saved = entry({ id: "new-entry", accountName: "person@example.com", iconEmoji });
      items = [...items, saved];
      return saved;
    }),
    cancelImport: vi.fn().mockResolvedValue(undefined),
    update: vi.fn().mockImplementation(async (request: MfaEntryUpdateRequest) => {
      const current = items.find((item) => item.id === request.id) ?? entry({ id: request.id });
      const saved = { ...current, ...request };
      items = items.map((item) => item.id === saved.id ? saved : item);
      return saved;
    }),
    delete: vi.fn().mockImplementation(async (id: string) => {
      items = items.filter((item) => item.id !== id);
    }),
    reorder: vi.fn().mockImplementation(async (orderedIds: string[]) => {
      const byId = new Map(items.map((item) => [item.id, item]));
      const reordered = orderedIds.map((id) => byId.get(id)!);
      items = [...reordered.filter((item) => item.pinned), ...reordered.filter((item) => !item.pinned)];
      return structuredClone(items);
    }),
    setPinned: vi.fn().mockImplementation(async (id: string, pinned: boolean) => {
      const current = items.find((item) => item.id === id)!;
      const saved = { ...current, pinned };
      const remaining = items.filter((item) => item.id !== id);
      items = pinned
        ? [saved, ...remaining.filter((item) => item.pinned), ...remaining.filter((item) => !item.pinned)]
        : [...remaining.filter((item) => item.pinned), saved, ...remaining.filter((item) => !item.pinned)];
      return structuredClone(items);
    }),
    reveal: vi.fn().mockResolvedValue({ id: entry().id, code: "123456", validUntil: Date.now() + 30_000 }),
    copy: vi.fn().mockResolvedValue(undefined),
    configureRecoveryPassword: vi.fn().mockResolvedValue(undefined),
    unlockWithRecoveryPassword: vi.fn().mockResolvedValue(undefined),
    lock: vi.fn().mockResolvedValue(undefined),
  } as MfaApi & Record<string, ReturnType<typeof vi.fn> | (() => boolean)>;
}

function pointerEvent(type: string, values: Record<string, number>): Event {
  const event = new Event(type, { bubbles: true, cancelable: true });
  for (const [key, value] of Object.entries(values)) {
    Object.defineProperty(event, key, { configurable: true, value });
  }
  return event;
}

function domRect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    x: left,
    y: top,
    top,
    right: left + width,
    bottom: top + height,
    left,
    width,
    height,
    toJSON: () => ({}),
  };
}

describe("MfaTool", () => {
  it("tells the user when the vault was recovered from a backup", async () => {
    const api = mockApi();
    (api.getStatus as ReturnType<typeof vi.fn>).mockResolvedValue({
      available: true,
      locked: false,
      entryCount: 1,
      protection: "windows-dpapi-recovery-password",
      recoveryState: "ready",
      captureExcluded: true,
      recoveredFromBackup: true,
      message: "MFA 主保险库缺失或损坏，已从最近的有效备份恢复。",
    });

    const rendered = render(MfaTool, { api });

    expect(await rendered.findByRole("status")).toHaveTextContent("已从最近的有效备份恢复");
  });

  it("blocks first use until a recovery password is configured", async () => {
    const api = mockApi([]);
    (api.getStatus as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      available: true,
      locked: false,
      entryCount: 0,
      protection: "windows-dpapi",
      recoveryState: "setup-required",
      captureExcluded: true,
    });
    const rendered = render(MfaTool, { api });
    const dialog = await rendered.findByRole("dialog", { name: "设置 MFA 恢复密码" });

    expect(within(dialog).getByText("本机使用仍然不需要输入密码")).toBeInTheDocument();
    await fireEvent.input(within(dialog).getByLabelText("恢复密码"), { target: { value: "correct horse battery" } });
    await fireEvent.input(within(dialog).getByLabelText("确认恢复密码"), { target: { value: "correct horse battery" } });
    await fireEvent.click(within(dialog).getByRole("button", { name: "设置恢复密码" }));

    await waitFor(() => expect(api.configureRecoveryPassword).toHaveBeenCalledWith("correct horse battery"));
    await waitFor(() => expect(rendered.queryByRole("dialog", { name: "设置 MFA 恢复密码" })).not.toBeInTheDocument());
  });

  it("validates recovery password length and confirmation before calling the API", async () => {
    const api = mockApi([]);
    (api.getStatus as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      available: true,
      locked: false,
      entryCount: 0,
      protection: "windows-dpapi",
      recoveryState: "setup-required",
      captureExcluded: true,
    });
    const rendered = render(MfaTool, { api });
    const dialog = await rendered.findByRole("dialog", { name: "设置 MFA 恢复密码" });
    const form = dialog.querySelector("form") as HTMLFormElement;
    const password = within(dialog).getByLabelText("恢复密码");
    const confirmation = within(dialog).getByLabelText("确认恢复密码");

    await fireEvent.input(password, { target: { value: "too-short" } });
    await fireEvent.input(confirmation, { target: { value: "too-short" } });
    await fireEvent.submit(form);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("至少需要 12 个字符");
    expect(api.configureRecoveryPassword).not.toHaveBeenCalled();

    await fireEvent.input(password, { target: { value: "correct horse battery" } });
    await fireEvent.input(confirmation, { target: { value: "different horse code" } });
    await fireEvent.submit(form);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("两次输入的恢复密码不一致");
    expect(api.configureRecoveryPassword).not.toHaveBeenCalled();
  });

  it("unlocks migrated MFA data with one recovery-password field", async () => {
    const api = mockApi([]);
    (api.getStatus as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      available: false,
      locked: true,
      entryCount: 0,
      protection: "windows-dpapi-recovery-password",
      recoveryState: "password-required",
      captureExcluded: true,
      message: "请输入恢复密码以迁移 MFA 数据。",
    });
    const rendered = render(MfaTool, { api });
    const dialog = await rendered.findByRole("dialog", { name: "使用恢复密码迁移" });

    expect(within(dialog).queryByLabelText("确认恢复密码")).not.toBeInTheDocument();
    await fireEvent.input(within(dialog).getByLabelText("恢复密码"), { target: { value: "correct horse battery" } });
    await fireEvent.click(within(dialog).getByRole("button", { name: "解锁并迁移" }));

    await waitFor(() => expect(api.unlockWithRecoveryPassword).toHaveBeenCalledWith("correct horse battery"));
    await waitFor(() => expect(rendered.queryByRole("dialog", { name: "使用恢复密码迁移" })).not.toBeInTheDocument());
  });

  it("changes the recovery password from the titlebar key action", async () => {
    const api = mockApi();
    const rendered = render(MfaTool, { api });
    await rendered.findByText("GitHub", { selector: ".account-heading strong" });

    await fireEvent.click(rendered.getByRole("button", { name: "修改 MFA 恢复密码" }));
    const dialog = rendered.getByRole("dialog", { name: "修改 MFA 恢复密码" });
    await fireEvent.input(within(dialog).getByLabelText("恢复密码"), { target: { value: "new recovery password" } });
    await fireEvent.input(within(dialog).getByLabelText("确认恢复密码"), { target: { value: "new recovery password" } });
    await fireEvent.click(within(dialog).getByRole("button", { name: "保存新密码" }));

    await waitFor(() => expect(api.configureRecoveryPassword).toHaveBeenCalledWith("new recovery password"));
    expect(rendered.queryByRole("dialog", { name: "修改 MFA 恢复密码" })).not.toBeInTheDocument();
  });

  it("keeps codes hidden by default, continuously reveals them, and hides on the second toggle", async () => {
    const api = mockApi();
    const rendered = render(MfaTool, { api });
    await rendered.findByText("GitHub", { selector: ".account-heading strong" });

    expect(rendered.getByLabelText("验证码已隐藏")).toHaveTextContent("••• •••");
    const toggle = rendered.getByRole("button", { name: "显示“GitHub”的验证码" });
    await fireEvent.click(toggle);
    await waitFor(() => expect(rendered.getByLabelText("验证码 123456")).toHaveTextContent("123 456"));
    expect(api.reveal).toHaveBeenCalledWith(entry().id);

    await fireEvent.click(rendered.getByRole("button", { name: "隐藏“GitHub”的验证码" }));
    expect(rendered.getByLabelText("验证码已隐藏")).toBeInTheDocument();
    expect(api.reveal).toHaveBeenCalledOnce();
  });

  it("refreshes every visible code from one shared clock after its TOTP period expires", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-08-01T08:00:00.000Z"));
    const api = mockApi();
    const reveal = api.reveal as ReturnType<typeof vi.fn>;
    reveal
      .mockResolvedValueOnce({ id: entry().id, code: "111111", validUntil: Date.now() + 900 })
      .mockResolvedValueOnce({ id: entry().id, code: "222222", validUntil: Date.now() + 30_000 });
    const rendered = render(MfaTool, { api });
    await vi.advanceTimersByTimeAsync(0);
    await Promise.resolve();

    await fireEvent.click(rendered.getByRole("button", { name: "显示“GitHub”的验证码" }));
    await vi.advanceTimersByTimeAsync(0);
    expect(rendered.getByLabelText("验证码 111111")).toBeInTheDocument();

    await vi.advanceTimersByTimeAsync(1_100);
    expect(reveal).toHaveBeenCalledTimes(2);
    expect(rendered.getByLabelText("验证码 222222")).toBeInTheDocument();
  });

  it("copies on row double-click without revealing or changing hidden state", async () => {
    const api = mockApi();
    const rendered = render(MfaTool, { api });
    const card = await rendered.findByLabelText("GitHub，双击复制验证码");

    await fireEvent.doubleClick(card);
    await waitFor(() => expect(api.copy).toHaveBeenCalledWith(entry().id));
    expect(api.reveal).not.toHaveBeenCalled();
    expect(rendered.getByLabelText("验证码已隐藏")).toBeInTheDocument();
  });

  it("reorders accounts from the dedicated drag handle without copying a code", async () => {
    const api = mockApi([
      entry(),
      entry({ id: "cloud", name: "Cloud", issuer: "Cloud", iconEmoji: "☁️" }),
      entry({ id: "mail", name: "Mail", issuer: "Mail", iconEmoji: "📧" }),
    ]);
    const rendered = render(MfaTool, { api });
    await rendered.findByText("Mail", { selector: ".account-heading strong" });

    const source = rendered.getByRole("button", { name: "调整“GitHub”的顺序" });
    const target = rendered.getByRole("button", { name: "调整“Mail”的顺序" });
    const sourceCard = source.closest<HTMLElement>(".account-card")!;
    const targetCard = target.closest<HTMLElement>(".account-card")!;
    vi.spyOn(sourceCard, "getBoundingClientRect").mockReturnValue(domRect(0, 100, 420, 88));
    vi.spyOn(targetCard, "getBoundingClientRect").mockReturnValue(domRect(0, 276, 420, 88));
    vi.spyOn(rendered.container.querySelector("main")!, "getBoundingClientRect").mockReturnValue(domRect(0, 44, 460, 500));
    const root = rendered.getByTestId("mfa-tool");

    await fireEvent(source, pointerEvent("pointerdown", { button: 0, pointerId: 17, clientX: 15, clientY: 120 }));
    expect(sourceCard).toHaveClass("dragging");
    await fireEvent(root, pointerEvent("pointermove", { pointerId: 17, clientX: 15, clientY: 350 }));
    expect(targetCard).toHaveClass("drop-after");
    await fireEvent(root, pointerEvent("pointerup", { pointerId: 17, clientX: 15, clientY: 350 }));

    await waitFor(() => expect(api.reorder).toHaveBeenCalledWith(["cloud", "mail", entry().id]));
    expect(api.copy).not.toHaveBeenCalled();
    const names = Array.from(rendered.container.querySelectorAll<HTMLElement>(".account-heading strong"))
      .map((element) => element.textContent);
    expect(names).toEqual(["Cloud", "Mail", "GitHub"]);
  });

  it("cancels a drag cleanly when pointer capture is lost", async () => {
    const api = mockApi([
      entry(),
      entry({ id: "cloud", name: "Cloud", issuer: "Cloud", iconEmoji: "☁️" }),
    ]);
    const rendered = render(MfaTool, { api });
    await rendered.findByText("Cloud", { selector: ".account-heading strong" });
    const handle = rendered.getByRole("button", { name: "调整“GitHub”的顺序" });
    const sourceCard = handle.closest<HTMLElement>(".account-card")!;
    const root = rendered.getByTestId("mfa-tool");

    await fireEvent(handle, pointerEvent("pointerdown", { button: 0, pointerId: 21, clientX: 10, clientY: 90 }));
    expect(sourceCard).toHaveClass("dragging");
    await fireEvent(root, pointerEvent("lostpointercapture", { pointerId: 21 }));
    expect(sourceCard).not.toHaveClass("dragging");
    await fireEvent(root, pointerEvent("pointerup", { pointerId: 21 }));
    expect(api.reorder).not.toHaveBeenCalled();
  });

  it("disables manual reordering while a search is active", async () => {
    const api = mockApi([
      entry(),
      entry({ id: "cloud", name: "Cloud", issuer: "Cloud", iconEmoji: "☁️" }),
    ]);
    const rendered = render(MfaTool, { api });
    await rendered.findByText("Cloud", { selector: ".account-heading strong" });

    await fireEvent.input(rendered.getByRole("searchbox", { name: "搜索账户" }), { target: { value: "Git" } });
    const handle = rendered.getByRole("button", { name: "调整“GitHub”的顺序" });
    expect(handle).toBeDisabled();
    expect(handle).toHaveAttribute("title", "清空搜索后可调整顺序");
    await fireEvent(handle, pointerEvent("pointerdown", { button: 0, pointerId: 3, clientX: 10, clientY: 90 }));
    await fireEvent(rendered.getByTestId("mfa-tool"), pointerEvent("pointerup", { pointerId: 3 }));
    expect(api.reorder).not.toHaveBeenCalled();
  });

  it("supports keyboard reordering and serializes list mutations while the order is saving", async () => {
    const github = entry();
    const cloud = entry({ id: "cloud", name: "Cloud", issuer: "Cloud", iconEmoji: "☁️" });
    const api = mockApi([github, cloud]);
    let finishReorder!: (entries: MfaEntrySummary[]) => void;
    (api.reorder as ReturnType<typeof vi.fn>).mockImplementationOnce(() => (
      new Promise<MfaEntrySummary[]>((resolve) => (finishReorder = resolve))
    ));
    const rendered = render(MfaTool, { api });
    await rendered.findByText("Cloud", { selector: ".account-heading strong" });

    const handle = rendered.getByRole("button", { name: "调整“GitHub”的顺序" });
    await fireEvent.keyDown(handle, { key: "ArrowDown", altKey: true });
    await waitFor(() => expect(api.reorder).toHaveBeenCalledWith([cloud.id, github.id]));
    expect(rendered.getByLabelText("验证器账户")).toHaveAttribute("aria-busy", "true");
    expect(rendered.getByRole("button", { name: "新增 MFA 账户" })).toBeDisabled();

    await fireEvent.contextMenu(rendered.getByLabelText("GitHub，双击复制验证码"), {
      clientX: 30,
      clientY: 30,
    });
    expect(rendered.getByRole("menuitem", { name: "编辑" })).toBeDisabled();
    expect(rendered.getByRole("menuitem", { name: "删除" })).toBeDisabled();

    finishReorder([cloud, github]);
    await waitFor(() => expect(rendered.getByLabelText("验证器账户")).toHaveAttribute("aria-busy", "false"));
    expect(rendered.getByRole("button", { name: "新增 MFA 账户" })).toBeEnabled();
  });

  it("pins and unpins an account from its context menu using backend order as authority", async () => {
    const api = mockApi([
      entry(),
      entry({ id: "cloud", name: "Cloud", issuer: "Cloud", iconEmoji: "☁️" }),
    ]);
    const rendered = render(MfaTool, { api });
    const cloudCard = await rendered.findByLabelText("Cloud，双击复制验证码");

    await fireEvent.contextMenu(cloudCard, { clientX: 30, clientY: 30 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: "置顶" }));
    await waitFor(() => expect(api.setPinned).toHaveBeenCalledWith("cloud", true));
    expect(rendered.getByLabelText("已置顶")).toBeInTheDocument();
    expect(rendered.container.querySelector(".account-heading strong")).toHaveTextContent("Cloud");

    const pinnedCard = rendered.getByLabelText("Cloud，双击复制验证码");
    await fireEvent.contextMenu(pinnedCard, { clientX: 30, clientY: 30 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: "取消置顶" }));
    await waitFor(() => expect(api.setPinned).toHaveBeenLastCalledWith("cloud", false));
    expect(rendered.queryByLabelText("已置顶")).not.toBeInTheDocument();
  });

  it("previews a standard link, shows warnings, imports it and securely asks the backend to copy", async () => {
    const api = mockApi([]);
    const uriPreview = preview({ warnings: ["链接中的 issuer 与标签不一致，请确认。"] });
    (api.previewUri as ReturnType<typeof vi.fn>).mockResolvedValue([uriPreview]);
    const rendered = render(MfaTool, { api });
    await rendered.findByText("还没有验证码");

    await fireEvent.click(rendered.getByRole("button", { name: "新增 MFA 账户" }));
    await fireEvent.click(rendered.getByRole("tab", { name: "粘贴链接" }));
    const uri = "otpauth://totp/GitHub%3Aperson%40example.com?secret=TEST&issuer=GitHub";
    await fireEvent.input(rendered.getByPlaceholderText("otpauth://totp/…"), { target: { value: uri } });
    await fireEvent.click(rendered.getByRole("button", { name: "识别链接" }));

    await waitFor(() => expect(api.previewUri).toHaveBeenCalledWith(uri));
    await rendered.findByText("确认识别结果");
    expect(rendered.getByText("链接中的 issuer 与标签不一致，请确认。")).toBeInTheDocument();
    await fireEvent.click(rendered.getByRole("button", { name: "添加并复制验证码" }));

    await waitFor(() => expect(api.commitImport).toHaveBeenCalledWith(uriPreview.sessionId, "🐙"));
    expect(api.copy).toHaveBeenCalledWith("new-entry");
    expect(rendered.queryByRole("dialog", { name: "添加验证器账户" })).not.toBeInTheDocument();
  });

  it("clears a manually entered secret immediately after creating an import preview", async () => {
    const api = mockApi([]);
    const rendered = render(MfaTool, { api });
    await rendered.findByText("还没有验证码");
    await fireEvent.click(rendered.getByRole("button", { name: "新增 MFA 账户" }));
    await fireEvent.click(rendered.getByRole("tab", { name: "手动输入" }));
    await fireEvent.input(rendered.getByPlaceholderText("例如：GitHub"), { target: { value: "Example" } });
    const secret = rendered.getByPlaceholderText("输入密钥，不含空格也可以");
    await fireEvent.input(secret, { target: { value: "JBSWY3DPEHPK3PXP" } });
    await fireEvent.click(rendered.getByRole("button", { name: "检查账户" }));

    await waitFor(() => expect(api.previewManual).toHaveBeenCalledWith(expect.objectContaining({
      name: "Example",
      secret: "JBSWY3DPEHPK3PXP",
    } satisfies Partial<MfaManualImportRequest>)));
    expect(rendered.container.textContent).not.toContain("JBSWY3DPEHPK3PXP");
  });

  it("offers edit and a confirmed delete from the account context menu", async () => {
    const api = mockApi();
    const rendered = render(MfaTool, { api });
    const card = await rendered.findByLabelText("GitHub，双击复制验证码");

    await fireEvent.contextMenu(card, { clientX: 30, clientY: 30 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: "编辑" }));
    const nameInput = rendered.getByLabelText("账户名称");
    await fireEvent.input(nameInput, { target: { value: "GitHub 工作" } });
    await fireEvent.click(rendered.getByRole("button", { name: "保存修改" }));
    await waitFor(() => expect(api.update).toHaveBeenCalledWith(expect.objectContaining({ name: "GitHub 工作" })));

    const editedCard = rendered.getByLabelText("GitHub 工作，双击复制验证码");
    await fireEvent.contextMenu(editedCard, { clientX: 30, clientY: 30 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: "删除" }));
    expect(rendered.getByRole("alertdialog", { name: "删除“GitHub 工作”？" })).toBeInTheDocument();
    expect(api.delete).not.toHaveBeenCalled();
    await fireEvent.click(rendered.getByRole("button", { name: "删除账户" }));
    await waitFor(() => expect(api.delete).toHaveBeenCalledWith(entry().id));
    expect(rendered.queryByText("GitHub 工作")).not.toBeInTheDocument();
  });

  it("does not bubble a reveal-button double-click into the row copy action", async () => {
    const api = mockApi();
    const rendered = render(MfaTool, { api });
    await rendered.findByText("GitHub", { selector: ".account-heading strong" });

    await fireEvent.doubleClick(rendered.getByRole("button", { name: "显示“GitHub”的验证码" }));
    expect(api.copy).not.toHaveBeenCalled();
  });

  it("supports keyboard copy and context-menu navigation from a focused account", async () => {
    const api = mockApi();
    const rendered = render(MfaTool, { api });
    const card = await rendered.findByRole("button", { name: "GitHub，双击复制验证码" });

    await fireEvent.keyDown(card, { key: "Enter" });
    await waitFor(() => expect(api.copy).toHaveBeenCalledWith(entry().id));

    await fireEvent.keyDown(card, { key: "ContextMenu" });
    const menu = rendered.getByRole("menu");
    await waitFor(() => expect(document.activeElement).toBe(menu.querySelector("button")));
    await fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).toHaveTextContent("显示验证码");
    await fireEvent.keyDown(menu, { key: "Escape" });
    expect(rendered.queryByRole("menu")).not.toBeInTheDocument();
    expect(document.activeElement).toBe(card);
  });

  it("moves focus with arrow keys across add-method tabs", async () => {
    const api = mockApi([]);
    const rendered = render(MfaTool, { api });
    await rendered.findByText("还没有验证码");
    await fireEvent.click(rendered.getByRole("button", { name: "新增 MFA 账户" }));

    const screenTab = rendered.getByRole("tab", { name: "扫描屏幕" });
    await waitFor(() => expect(document.activeElement).toBe(screenTab));
    await fireEvent.keyDown(screenTab, { key: "ArrowRight" });
    const uriTab = rendered.getByRole("tab", { name: "粘贴链接" });
    await waitFor(() => expect(document.activeElement).toBe(uriTab));
    expect(uriTab).toHaveAttribute("aria-selected", "true");
  });

  it("shows an unavailable vault message instead of an empty-account prompt", async () => {
    const api = mockApi([]);
    (api.getStatus as ReturnType<typeof vi.fn>).mockResolvedValue({
      available: false,
      locked: true,
      entryCount: 0,
      protection: "unavailable",
      recoveryState: "unavailable",
      captureExcluded: true,
      message: "当前 Windows 用户无法解锁这份 MFA 数据。",
    });
    const rendered = render(MfaTool, { api });

    expect(await rendered.findByText("MFA 数据保险库不可用")).toBeInTheDocument();
    expect(rendered.getByText("当前 Windows 用户无法解锁这份 MFA 数据。")).toBeInTheDocument();
    expect(rendered.queryByText("还没有验证码")).not.toBeInTheDocument();
    expect(rendered.getByRole("button", { name: "新增 MFA 账户" })).toBeDisabled();
  });

  it("filters by name, issuer and account", async () => {
    const api = mockApi([
      entry(),
      entry({ id: "cloud", name: "Cloud", issuer: "Example Corp", accountName: "alice@example.com", iconEmoji: "☁️" }),
    ]);
    const rendered = render(MfaTool, { api });
    await rendered.findByText("Cloud", { selector: ".account-heading strong" });

    await fireEvent.input(rendered.getByRole("searchbox", { name: "搜索账户" }), { target: { value: "alice" } });
    expect(rendered.getByText("Cloud", { selector: ".account-heading strong" })).toBeInTheDocument();
    expect(rendered.queryByText("GitHub", { selector: ".account-heading strong" })).not.toBeInTheDocument();
  });
});
