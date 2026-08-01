import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  MfaApi,
  MfaEntrySummary,
  MfaEntryUpdateRequest,
  MfaImportPreview,
  MfaManualImportRequest,
} from "../mfa";
import MfaTool from "./MfaTool.svelte";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function entry(overrides: Partial<MfaEntrySummary> = {}): MfaEntrySummary {
  return {
    id: "550e8400-e29b-41d4-a716-446655440000",
    name: "GitHub",
    issuer: "GitHub",
    accountName: "octocat",
    iconEmoji: "🐙",
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
      protection: "windows-dpapi",
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
    reveal: vi.fn().mockResolvedValue({ id: entry().id, code: "123456", validUntil: Date.now() + 30_000 }),
    copy: vi.fn().mockResolvedValue(undefined),
    lock: vi.fn().mockResolvedValue(undefined),
  } as MfaApi & Record<string, ReturnType<typeof vi.fn> | (() => boolean)>;
}

describe("MfaTool", () => {
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
      protection: "windows-dpapi",
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
