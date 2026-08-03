import { cleanup, fireEvent, render, waitFor, within } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { notesApi } from "../bridge";
import { createBrowserPasswordApi, type PasswordApi, type PasswordStatus } from "../passwords";
import PasswordManagerTool from "./PasswordManagerTool.svelte";

const eventHarness = vi.hoisted(() => {
  const listeners = new Map<string, Set<(event: { payload: unknown }) => void>>();
  return {
    listeners,
    listen: vi.fn(async (event: string, listener: (event: { payload: unknown }) => void) => {
      const eventListeners = listeners.get(event) ?? new Set();
      eventListeners.add(listener);
      listeners.set(event, eventListeners);
      return () => eventListeners.delete(listener);
    }),
    emit(event: string, payload: unknown = null) {
      for (const listener of listeners.get(event) ?? []) listener({ payload });
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: eventHarness.listen }));

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  eventHarness.listeners.clear();
  eventHarness.listen.mockClear();
});

describe("PasswordManagerTool", () => {
  it("renders searchable multi-account entries and records first-use capture consent", async () => {
    const api = createBrowserPasswordApi();
    const setCaptureEnabled = vi.spyOn(api, "setCaptureEnabled");
    const view = render(PasswordManagerTool, { api });

    expect(await view.findByText("personal@example.com")).toBeInTheDocument();
    expect(view.getByText("demo@example.com")).toBeInTheDocument();
    expect(view.getByRole("region", { name: "登录信息检测授权" })).toBeInTheDocument();

    await fireEvent.input(view.getByRole("searchbox", { name: "搜索密码账户" }), { target: { value: "personal@" } });
    expect(view.getByText("personal@example.com")).toBeInTheDocument();
    expect(view.queryByText("demo@contoso.example")).not.toBeInTheDocument();

    await fireEvent.click(view.getByRole("button", { name: "开启检测" }));
    await waitFor(() => expect(setCaptureEnabled).toHaveBeenCalledWith(true));
    expect(await view.findByText("允许登录信息检测")).toBeInTheDocument();
  });

  it("adds an account with generated password controls and explicit HTTP consent", async () => {
    const api = createBrowserPasswordApi();
    const create = vi.spyOn(api, "create");
    const view = render(PasswordManagerTool, { api });
    await view.findByText("demo@example.com");

    await fireEvent.click(view.getByRole("button", { name: "添加账户" }));
    await fireEvent.input(view.getByLabelText("站点名称"), { target: { value: "内部系统" } });
    await fireEvent.input(view.getByLabelText("登录网址"), { target: { value: "http://intranet.example/login" } });
    await fireEvent.input(view.getByLabelText("用户名"), { target: { value: "worker" } });
    expect(view.getByText(/HTTP 连接不会加密传输/)).toBeInTheDocument();
    await fireEvent.click(view.getByLabelText("允许这个 HTTP origin（仅逐站点生效）"));
    await fireEvent.click(view.getByRole("button", { name: "密码生成器" }));
    await fireEvent.click(view.getByRole("button", { name: "生成并使用" }));
    await fireEvent.click(view.getByRole("button", { name: "保存账户" }));

    await waitFor(() => expect(create).toHaveBeenCalledWith(expect.objectContaining({
      siteName: "内部系统",
      loginUrl: "http://intranet.example/login",
      username: "worker",
      allowInsecureHttp: true,
      password: expect.any(String),
    })));
    expect(await view.findByText("内部系统")).toBeInTheDocument();
    expect(view.getByText("HTTP 不安全")).toBeInTheDocument();
  });

  it("reveals a saved password only through the short-lived API response", async () => {
    const api = createBrowserPasswordApi();
    const reveal = vi.spyOn(api, "reveal");
    const view = render(PasswordManagerTool, { api });
    await view.findByText("demo@example.com");

    await fireEvent.click(view.getAllByRole("button", { name: "显示密码" })[0]);
    expect(await view.findByText("Demo-Google-2026!")).toBeInTheDocument();
    expect(reveal).toHaveBeenCalledWith("browser-demo-google-work");

    await fireEvent.click(view.getAllByRole("button", { name: "隐藏密码" })[0]);
    expect(view.queryByText("Demo-Google-2026!")).not.toBeInTheDocument();
  });

  it("opens the site directly without requesting fill when Firefox is disconnected", async () => {
    const api = createBrowserPasswordApi();
    const startFill = vi.spyOn(api, "startFill");
    const openExternalLink = vi.spyOn(notesApi, "openExternalLink").mockResolvedValue(undefined);
    const view = render(PasswordManagerTool, { api });
    await view.findByText("demo@example.com");

    await fireEvent.click(view.getAllByRole("button", { name: "打开站点 Google Workspace" })[0]);

    await waitFor(() => expect(openExternalLink).toHaveBeenCalledWith("https://accounts.google.com/"));
    expect(startFill).not.toHaveBeenCalled();
    expect(await view.findByText(/可从飞花复制账号和密码/)).toBeInTheDocument();
  });

  it("does not report fill as started while Firefox toolbar authorization is required", async () => {
    const base = createBrowserPasswordApi();
    const browser = {
      browser: "firefox" as const,
      connection: "connected" as const,
      extensionInstalled: true,
      nativeHostInstalled: true,
      capturePermission: "action-required" as const,
    };
    const startFill = vi.fn().mockResolvedValue({
      sessionId: "fill-1",
      entryId: "browser-demo-google-work",
      browser: "firefox",
      origin: "https://accounts.google.com",
      expiresAt: Date.now() + 60_000,
      actionRequired: "toolbar-click",
    });
    const api: PasswordApi = {
      ...base,
      getBrowserStatus: vi.fn().mockResolvedValue(browser),
      startFill,
    };
    const view = render(PasswordManagerTool, { api });
    await view.findByText("demo@example.com");

    await fireEvent.click(view.getAllByRole("button", { name: "授权后填充 Google Workspace" })[0]);

    await waitFor(() => expect(startFill).toHaveBeenCalledWith("browser-demo-google-work"));
    expect(startFill).toHaveBeenCalledTimes(1);
    expect(await view.findByText("Firefox 密码权限等待授权")).toBeInTheDocument();
    expect(view.getByRole("alert")).toHaveTextContent("授权后回到已打开的页面确认填充，无需再次点击");
    expect(view.queryByText(/已在 Firefox 中打开/)).not.toBeInTheDocument();
  });

  it("starts and cancels template recording when Firefox is connected", async () => {
    const base = createBrowserPasswordApi();
    const connected = {
      browser: "firefox" as const,
      connection: "connected" as const,
      extensionInstalled: true,
      nativeHostInstalled: true,
      extensionVersion: "0.6.0",
      installUrl: null,
      capturePermission: "granted" as const,
    };
    const startTemplateRecording = vi.fn().mockResolvedValue({
      sessionId: "recording-1",
      entryId: "browser-demo-google-work",
      origin: "https://accounts.google.com",
      state: "recording",
      expiresAt: Date.now() + 60_000,
    });
    const cancelTemplateRecording = vi.fn().mockResolvedValue(undefined);
    const api: PasswordApi = {
      ...base,
      getBrowserStatus: vi.fn().mockResolvedValue(connected),
      startTemplateRecording,
      cancelTemplateRecording,
    };
    const view = render(PasswordManagerTool, { api });
    await view.findByText("demo@example.com");

    await fireEvent.click(view.getAllByRole("button", { name: "录制 Google Workspace 模板" })[0]);
    await waitFor(() => expect(startTemplateRecording).toHaveBeenCalledWith("browser-demo-google-work"));
    expect(await view.findByText("正在录制站点模板")).toBeInTheDocument();

    await fireEvent.click(view.getByRole("button", { name: "取消录制" }));
    await waitFor(() => expect(cancelTemplateRecording).toHaveBeenCalledWith("recording-1"));
    expect(view.queryByText("正在录制站点模板")).not.toBeInTheDocument();
  });

  it("requires recovery setup before showing vault entries", async () => {
    const base = createBrowserPasswordApi();
    const status: PasswordStatus = {
      available: true,
      locked: false,
      entryCount: 0,
      protection: "windows-dpapi",
      recoveryState: "setup-required",
      captureEnabled: false,
      captureConfigured: false,
      browser: await base.getBrowserStatus(),
    };
    const configureRecoveryPassword = vi.fn().mockResolvedValue(undefined);
    const api: PasswordApi = {
      ...base,
      getStatus: vi.fn().mockResolvedValue(status),
      configureRecoveryPassword,
    };
    const view = render(PasswordManagerTool, { api });

    expect(await view.findByRole("dialog", { name: "设置恢复密码" })).toBeInTheDocument();
    const inputs = view.getAllByLabelText(/新的恢复密码|再次输入/);
    await fireEvent.input(inputs[0], { target: { value: "correct horse battery" } });
    await fireEvent.input(inputs[1], { target: { value: "correct horse battery" } });
    await fireEvent.click(view.getByRole("button", { name: "设置并继续" }));
    await waitFor(() => expect(configureRecoveryPassword).toHaveBeenCalledWith("correct horse battery"));
  });

  it("shows the unsupported platform state without opening recovery setup", async () => {
    const base = createBrowserPasswordApi();
    const status: PasswordStatus = {
      available: false,
      locked: true,
      entryCount: 0,
      protection: "unavailable",
      recoveryState: "setup-required",
      captureEnabled: false,
      captureConfigured: false,
      browser: {
        browser: "firefox",
        connection: "unsupported",
        extensionInstalled: false,
        nativeHostInstalled: false,
      },
    };
    const api: PasswordApi = {
      ...base,
      getStatus: vi.fn().mockResolvedValue(status),
      getBrowserStatus: vi.fn().mockResolvedValue(status.browser),
    };
    const view = render(PasswordManagerTool, { api });

    expect(await view.findByText("密码保险库不可用")).toBeInTheDocument();
    expect(view.getByText(/首版仅支持 Windows/)).toBeInTheDocument();
    expect(view.queryByRole("dialog", { name: "设置恢复密码" })).not.toBeInTheDocument();
  });

  it("shows a locked state after explicit lock and unlocks in the same window", async () => {
    const base = createBrowserPasswordApi();
    let status: PasswordStatus = {
      available: true,
      locked: false,
      entryCount: 3,
      protection: "windows-dpapi-recovery-password",
      recoveryState: "ready",
      captureEnabled: false,
      captureConfigured: true,
      browser: await base.getBrowserStatus(),
    };
    const lock = vi.fn().mockImplementation(async () => {
      status = { ...status, locked: true, recoveryState: "password-required" };
    });
    const unlockWithRecoveryPassword = vi.fn().mockImplementation(async () => {
      status = { ...status, locked: false, recoveryState: "ready" };
    });
    const api: PasswordApi = {
      ...base,
      getStatus: vi.fn().mockImplementation(async () => structuredClone(status)),
      lock,
      unlockWithRecoveryPassword,
    };
    const view = render(PasswordManagerTool, { api });
    await view.findByText("demo@example.com");

    await fireEvent.click(view.getByRole("button", { name: "锁定保险库" }));
    expect(await view.findByTestId("password-locked-state")).toBeInTheDocument();
    expect(lock).toHaveBeenCalledOnce();

    await fireEvent.click(view.getByRole("button", { name: "输入恢复密码" }));
    const dialog = view.getByRole("dialog", { name: "解锁密码保险库" });
    await fireEvent.input(within(dialog).getByLabelText(/恢复密码/), { target: { value: "correct horse battery" } });
    await fireEvent.click(within(dialog).getByRole("button", { name: "解锁" }));
    await waitFor(() => expect(unlockWithRecoveryPassword).toHaveBeenCalledWith("correct horse battery"));
    expect(await view.findByText("demo@example.com")).toBeInTheDocument();
  });

  it("reports a manual lock failure without discarding the current window", async () => {
    const base = createBrowserPasswordApi();
    const status: PasswordStatus = {
      available: true,
      locked: false,
      entryCount: 3,
      protection: "windows-dpapi-recovery-password",
      recoveryState: "ready",
      captureEnabled: false,
      captureConfigured: true,
      browser: await base.getBrowserStatus(),
    };
    const lock = vi.fn().mockRejectedValue(new Error("保险库锁定失败"));
    const api: PasswordApi = {
      ...base,
      getStatus: vi.fn().mockResolvedValue(status),
      lock,
    };
    const view = render(PasswordManagerTool, { api });
    await view.findByText("demo@example.com");

    await fireEvent.click(view.getByRole("button", { name: "锁定保险库" }));

    expect(await view.findByRole("alert")).toHaveTextContent("保险库锁定失败");
    expect(lock).toHaveBeenCalledOnce();
  });

  it("immediately removes revealed and edited secrets after an idle-lock event", async () => {
    const base = createBrowserPasswordApi();
    let status: PasswordStatus = {
      available: true,
      locked: false,
      entryCount: 3,
      protection: "windows-dpapi-recovery-password",
      recoveryState: "ready",
      captureEnabled: true,
      captureConfigured: true,
      browser: await base.getBrowserStatus(),
    };
    const api: PasswordApi = {
      ...base,
      isDesktop: () => true,
      getStatus: vi.fn().mockImplementation(async () => structuredClone(status)),
    };
    const view = render(PasswordManagerTool, { api });
    await view.findByText("demo@example.com");
    await waitFor(() => expect(eventHarness.listen).toHaveBeenCalledWith(
      "password-vault-locked",
      expect.any(Function),
    ));

    await fireEvent.click(view.getAllByRole("button", { name: "显示密码" })[0]);
    expect(await view.findByText("Demo-Google-2026!")).toBeInTheDocument();
    await fireEvent.click(view.getAllByRole("button", { name: "编辑 Google Workspace" })[0]);
    expect(view.getByRole("dialog", { name: "编辑密码账户" })).toBeInTheDocument();

    status = { ...status, locked: true, recoveryState: "password-required", captureEnabled: false };
    eventHarness.emit("password-vault-locked");

    await waitFor(() => {
      expect(view.queryByText("Demo-Google-2026!")).not.toBeInTheDocument();
      expect(view.queryByRole("dialog", { name: "编辑密码账户" })).not.toBeInTheDocument();
      expect(view.queryByText("demo@example.com")).not.toBeInTheDocument();
      expect(view.getByTestId("password-locked-state")).toBeInTheDocument();
    });
  });
});
