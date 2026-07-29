import { afterEach, describe, expect, it, vi } from "vitest";
import pageSource from "./+page.svelte?raw";

type NotesApi = typeof import("$lib/bridge").notesApi;
let activeUnmount: (() => void) | null = null;

afterEach(() => {
  activeUnmount?.();
  activeUnmount = null;
  document.documentElement.classList.remove("timer-tool-page");
  document.documentElement.classList.remove("timer-tool-window");
  document.documentElement.classList.remove("screenshot-tool-page");
  document.documentElement.classList.remove("pinned-screenshot-page");
  document.documentElement.classList.remove("transparent-tool-page");
  document.body.classList.remove("timer-tool-page");
  document.body.classList.remove("timer-tool-window");
  document.body.classList.remove("screenshot-tool-page");
  document.body.classList.remove("pinned-screenshot-page");
  document.body.classList.remove("transparent-tool-page");
  window.history.replaceState({}, "", "/");
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function watchNotesApi(notesApi: NotesApi) {
  return {
    appInfo: vi.spyOn(notesApi, "appInfo"),
    listNotes: vi.spyOn(notesApi, "listNotes"),
    listTrash: vi.spyOn(notesApi, "listTrash"),
    getNote: vi.spyOn(notesApi, "getNote"),
    createNote: vi.spyOn(notesApi, "createNote"),
  };
}

function expectNotesApiUntouched(spies: ReturnType<typeof watchNotesApi>): void {
  expect(spies.appInfo).not.toHaveBeenCalled();
  expect(spies.listNotes).not.toHaveBeenCalled();
  expect(spies.listTrash).not.toHaveBeenCalled();
  expect(spies.getNote).not.toHaveBeenCalled();
  expect(spies.createNote).not.toHaveBeenCalled();
}

async function renderTool(tool: "timer" | "reminder" | "gantt" | "screenshot") {
  vi.resetModules();
  window.history.replaceState({}, "", `/?tool=${tool}`);
  if (tool === "screenshot") {
    vi.stubGlobal("ResizeObserver", class {
      observe(): void {}
      disconnect(): void {}
    });
  }
  const [{ render }, { notesApi }, { screenshotApi }, { default: Page }] = await Promise.all([
    import("@testing-library/svelte"),
    import("$lib/bridge"),
    import("$lib/screenshot"),
    import("./+page.svelte"),
  ]);
  if (tool === "screenshot") vi.spyOn(screenshotApi, "getSession").mockResolvedValue(null);
  const api = watchNotesApi(notesApi);
  const rendered = render(Page);
  activeUnmount = () => rendered.unmount();
  return { api, rendered };
}

describe("tool pages", () => {
  it("renders only the timer without initializing the notes workspace", async () => {
    const { api, rendered } = await renderTool("timer");

    expect(rendered.getByTestId("timer-tool")).toBeInTheDocument();
    expect(document.title).toBe("计时器 - 飞花 - PetalDesk");
    expect(rendered.container.querySelector(".reminder-tool-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".main-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".startup-state")).not.toBeInTheDocument();
    expect(document.documentElement).toHaveClass("timer-tool-page");
    expect(document.body).toHaveClass("timer-tool-page");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
    expectNotesApiUntouched(api);

    rendered.unmount();
    activeUnmount = null;
    expect(document.documentElement).not.toHaveClass("timer-tool-page");
    expect(document.body).not.toHaveClass("timer-tool-page");
  }, 60_000);

  it("renders only the reminder with an opaque page and no notes initialization", async () => {
    const { api, rendered } = await renderTool("reminder");

    expect(rendered.getByTestId("reminder-tool")).toBeInTheDocument();
    expect(document.title).toBe("提醒 - 飞花 - PetalDesk");
    expect(rendered.container.querySelector(".reminder-tool-window")).toBeInTheDocument();
    expect(rendered.container.querySelector(".timer-tool-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".main-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".startup-state")).not.toBeInTheDocument();
    expect(document.documentElement).not.toHaveClass("timer-tool-page");
    expect(document.body).not.toHaveClass("timer-tool-page");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
    expectNotesApiUntouched(api);
  }, 60_000);

  it("renders only the gantt tool with an opaque page and no notes initialization", async () => {
    const { api, rendered } = await renderTool("gantt");

    expect(await rendered.findByTestId("gantt-tool")).toBeInTheDocument();
    expect(document.title).toBe("任务甘特图 - 飞花 - PetalDesk");
    expect(rendered.container.querySelector(".gantt-tool-window")).toBeInTheDocument();
    expect(rendered.container.querySelector(".timer-tool-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".reminder-tool-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".main-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".startup-state")).not.toBeInTheDocument();
    expect(document.documentElement).not.toHaveClass("timer-tool-page");
    expect(document.body).not.toHaveClass("timer-tool-page");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
    expectNotesApiUntouched(api);
  }, 60_000);

  it("lazily renders the screenshot capture page without initializing notes", async () => {
    const { api, rendered } = await renderTool("screenshot");

    expect(await rendered.findByTestId("screenshot-tool")).toBeInTheDocument();
    expect(document.title).toBe("截图 - 飞花 - PetalDesk");
    expect(rendered.container.querySelector(".screenshot-tool-window")).toBeInTheDocument();
    expect(rendered.container.querySelector(".main-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".startup-state")).not.toBeInTheDocument();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
    expectNotesApiUntouched(api);
  }, 60_000);

  it("prioritizes a pinned screenshot route and does not initialize notes", async () => {
    vi.resetModules();
    window.history.replaceState({}, "", "/?tool=timer&screenshotPin=pin-1");
    const [{ render }, { notesApi }, { pinnedScreenshotApi }, { default: Page }] = await Promise.all([
      import("@testing-library/svelte"),
      import("$lib/bridge"),
      import("$lib/screenshot"),
      import("./+page.svelte"),
    ]);
    vi.spyOn(pinnedScreenshotApi, "getPng").mockRejectedValue(new Error("测试贴图"));
    const api = watchNotesApi(notesApi);
    const rendered = render(Page);
    activeUnmount = () => rendered.unmount();

    expect(await rendered.findByTestId("pinned-screenshot")).toBeInTheDocument();
    expect(document.title).toBe("贴图 - 飞花 - PetalDesk");
    expect(rendered.container.querySelector(".pinned-screenshot-window")).toBeInTheDocument();
    expect(rendered.container.querySelector(".timer-tool-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".main-window")).not.toBeInTheDocument();
    expectNotesApiUntouched(api);
  }, 60_000);

  it("renders a transparent long-capture outline without initializing application content", async () => {
    vi.resetModules();
    window.history.replaceState({}, "", "/?tool=screenshot&longOutline=long-42");
    const [{ render }, { notesApi }, { default: Page }] = await Promise.all([
      import("@testing-library/svelte"),
      import("$lib/bridge"),
      import("./+page.svelte"),
    ]);
    const api = watchNotesApi(notesApi);
    const rendered = render(Page);
    activeUnmount = () => rendered.unmount();

    const outline = rendered.getByTestId("long-capture-outline");
    expect(outline).toBeInTheDocument();
    expect(outline).toHaveTextContent("");
    expect(outline.childElementCount).toBe(0);
    expect(document.title).toBe("长截图范围 - 飞花 - PetalDesk");
    expect(document.documentElement).toHaveClass("transparent-tool-page");
    expect(document.body).toHaveClass("transparent-tool-page");
    expect(pageSource).toMatch(
      /\.long-capture-outline-window\s*\{[^}]*position:\s*fixed;[^}]*background:\s*transparent;[^}]*pointer-events:\s*none;/,
    );
    expect(pageSource).toMatch(
      /:global\(html\.transparent-tool-page\),\s*:global\(body\.transparent-tool-page\)\s*\{[^}]*background:\s*transparent\s*!important;/,
    );
    expect(rendered.container.querySelector(".screenshot-tool-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".long-capture-control-window")).not.toBeInTheDocument();
    expect(rendered.container.querySelector(".main-window")).not.toBeInTheDocument();
    expectNotesApiUntouched(api);

    rendered.unmount();
    activeUnmount = null;
    expect(document.documentElement).not.toHaveClass("transparent-tool-page");
    expect(document.body).not.toHaveClass("transparent-tool-page");
  }, 60_000);
});
