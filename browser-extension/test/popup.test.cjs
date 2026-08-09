const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

function loadPopup({ tab, onMessage } = {}) {
  const sentMessages = [];
  let currentTab = tab === undefined
    ? {
      accounts: [
        { entryId: "entry-a", hasMfa: false, siteName: "Example", username: "alice" },
        { entryId: "entry-b", hasMfa: true, siteName: "Example", username: "bob" },
      ],
      locked: false,
      origin: "https://example.test",
    }
    : tab;
  let document;

  class FakeElement {
    constructor(tagName = "div") {
      this.tagName = String(tagName).toUpperCase();
      this.children = [];
      this.listeners = new Map();
      this.attributes = new Map();
      this.textContent = "";
      this.className = "";
      this.disabled = false;
      this.id = "";
      this.parentElement = null;
      this.type = "";
    }

    get firstChild() {
      return this.children[0] || null;
    }

    append(...children) {
      for (const child of children) {
        child.parentElement = this;
        this.children.push(child);
      }
    }

    appendChild(child) {
      this.append(child);
      return child;
    }

    removeChild(child) {
      const index = this.children.indexOf(child);
      if (index !== -1) this.children.splice(index, 1);
      child.parentElement = null;
      return child;
    }

    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) || [];
      listeners.push(listener);
      this.listeners.set(type, listeners);
    }

    setAttribute(name, value) {
      this.attributes.set(name, String(value));
    }

    getAttribute(name) {
      return this.attributes.has(name) ? this.attributes.get(name) : null;
    }

    removeAttribute(name) {
      this.attributes.delete(name);
    }

    focus() {
      document.activeElement = this;
    }

    dispatch(type, options = {}) {
      let prevented = false;
      let stopped = false;
      const event = {
        ...options,
        currentTarget: this,
        target: options.target || this,
        type,
        preventDefault() { prevented = true; },
        stopPropagation() { stopped = true; },
      };
      for (const listener of this.listeners.get(type) || []) listener(event);
      return { prevented, stopped };
    }

    click() {
      if (this.disabled) return;
      this.focus();
      this.dispatch("click");
    }
  }

  const elements = new Map();
  const elementTags = {
    "account-list": "ul",
    "diagnostics-list": "ul",
    "open-manager": "button",
  };
  for (const id of [
    "diagnostics-list",
    "site-origin",
    "site-status",
    "account-list",
    "open-manager",
    "extension-version",
  ]) {
    const element = new FakeElement(elementTags[id] || "p");
    element.id = id;
    elements.set(id, element);
  }

  const documentListeners = new Map();
  document = {
    activeElement: null,
    createElement(tagName) {
      return new FakeElement(tagName);
    },
    getElementById(id) {
      return elements.get(id) || null;
    },
    addEventListener(type, listener) {
      const listeners = documentListeners.get(type) || [];
      listeners.push(listener);
      documentListeners.set(type, listeners);
    },
  };

  const runtime = {
    sendMessage(message) {
      const serialized = JSON.parse(JSON.stringify(message));
      sentMessages.push(serialized);
      if (typeof onMessage === "function") {
        const result = onMessage(serialized, {
          getTab: () => currentTab,
          setTab: (nextTab) => { currentTab = nextTab; },
        });
        if (result !== undefined) return Promise.resolve(result);
      }
      if (message.type === "petaldesk.popup.getState") {
        return Promise.resolve({
          diagnostics: { captureEnabled: true, extensionVersion: "0.8.1" },
          tab: currentTab,
        });
      }
      if (message.type === "petaldesk.popup.deleteEntry") {
        currentTab = {
          ...currentTab,
          accounts: currentTab.accounts.filter((account) => account.entryId !== message.entryId),
        };
      }
      return Promise.resolve({ accepted: true });
    },
  };
  const context = {
    browser: { runtime },
    clearTimeout,
    console,
    document,
    setTimeout,
  };
  context.globalThis = context;
  vm.createContext(context);
  vm.runInContext(
    fs.readFileSync(path.join(__dirname, "..", "src", "popup", "popup.js"), "utf8"),
    context,
    { filename: "src/popup/popup.js" },
  );

  const accountList = elements.get("account-list");

  function descendants(element, predicate, found = []) {
    for (const child of element.children) {
      if (predicate(child)) found.push(child);
      descendants(child, predicate, found);
    }
    return found;
  }

  function hasClass(element, className) {
    return element.className.split(/\s+/).includes(className);
  }

  function buttonInCard(cardIndex, label) {
    const card = accountList.children[cardIndex];
    assert.ok(card, `account card ${cardIndex} should exist`);
    const button = descendants(
      card,
      (candidate) => candidate.tagName === "BUTTON"
        && (candidate.getAttribute("aria-label") === label || candidate.textContent === label),
    )[0];
    assert.ok(button, `the ${label} button should exist in account card ${cardIndex}`);
    return button;
  }

  return {
    accountList,
    document,
    elements,
    sentMessages,
    buttonInCard,
    card(index) {
      const card = accountList.children[index];
      assert.ok(card, `account card ${index} should exist`);
      return card;
    },
    contextMenus() {
      return descendants(accountList, (element) => hasClass(element, "account-context-menu"));
    },
    dialogs() {
      return descendants(accountList, (element) => hasClass(element, "delete-confirm"));
    },
    dispatchContextMenu(cardIndex, target = null) {
      const card = accountList.children[cardIndex];
      assert.ok(card, `account card ${cardIndex} should exist`);
      return card.dispatch("contextmenu", { target: target || card });
    },
    dispatchContextKey(cardIndex, key, { shiftKey = false, target = null } = {}) {
      const card = accountList.children[cardIndex];
      assert.ok(card, `account card ${cardIndex} should exist`);
      return card.dispatch("keydown", { key, shiftKey, target: target || card });
    },
    dispatchEscape() {
      let prevented = false;
      for (const listener of documentListeners.get("keydown") || []) {
        listener({
          key: "Escape",
          preventDefault() { prevented = true; },
        });
      }
      return prevented;
    },
    dispatchDocumentPointerDown(target = null) {
      for (const listener of documentListeners.get("pointerdown") || []) {
        listener({ target });
      }
    },
    iconButtons(cardIndex) {
      const card = accountList.children[cardIndex];
      return descendants(card, (element) => element.tagName === "BUTTON" && hasClass(element, "icon-button"));
    },
    nestedButtons() {
      const buttons = descendants(accountList, (element) => element.tagName === "BUTTON");
      return buttons.filter((button) => descendants(button, (element) => element.tagName === "BUTTON").length > 0);
    },
    status() {
      return elements.get("site-status").textContent;
    },
  };
}

function flush() {
  return new Promise((resolve) => setImmediate(resolve));
}

test("account cards keep three copy actions and no delete icon or nested buttons", async () => {
  const harness = loadPopup();
  await flush();

  assert.equal(harness.accountList.children.length, 2);
  assert.equal(harness.nestedButtons().length, 0);
  assert.deepEqual(
    harness.iconButtons(0).map((button) => button.getAttribute("aria-label")),
    ["复制用户名", "复制密码", "未关联 MFA"],
  );
  assert.deepEqual(
    harness.iconButtons(1).map((button) => button.getAttribute("aria-label")),
    ["复制用户名", "复制密码", "复制 MFA 验证码"],
  );
  assert.equal(harness.buttonInCard(0, "未关联 MFA").disabled, true);
  assert.equal(harness.buttonInCard(0, "未关联 MFA").getAttribute("title"), "未关联 MFA");

  const source = fs.readFileSync(path.join(__dirname, "..", "src", "popup", "popup.js"), "utf8");
  assert.equal(source.includes("⋯"), false);
  assert.match(source, /account-context-menu/);
  assert.equal(source.includes("delete-trigger"), false);
  assert.equal(source.includes("trash-2"), false);
  assert.equal(source.includes("复制账号"), false);
});

test("copy icons send the selected field without triggering password fill", async () => {
  const harness = loadPopup();
  await flush();

  harness.buttonInCard(0, "复制用户名").click();
  await flush();
  assert.deepEqual(harness.sentMessages.at(-1), {
    type: "petaldesk.popup.copySecret",
    entryId: "entry-a",
    field: "username",
  });
  assert.equal(harness.status(), "已复制用户名");

  harness.buttonInCard(0, "复制密码").click();
  await flush();
  assert.deepEqual(harness.sentMessages.at(-1), {
    type: "petaldesk.popup.copySecret",
    entryId: "entry-a",
    field: "password",
  });
  assert.equal(harness.status(), "已复制密码");
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.fill"),
    false,
  );
});

test("the reserved MFA slot is inert while linked MFA copies through the background", async () => {
  const harness = loadPopup();
  await flush();

  const before = harness.sentMessages.length;
  harness.buttonInCard(0, "未关联 MFA").click();
  assert.equal(harness.sentMessages.length, before);

  harness.buttonInCard(1, "复制 MFA 验证码").click();
  await flush();
  assert.deepEqual(harness.sentMessages.at(-1), {
    type: "petaldesk.popup.copySecret",
    entryId: "entry-b",
    field: "mfa",
  });
  assert.equal(harness.status(), "MFA 验证码已复制，过期后自动清除");
});

test("the account body remains the only fill action", async () => {
  const harness = loadPopup();
  await flush();

  harness.buttonInCard(1, "填充 Example 的账户").click();
  await flush();
  assert.deepEqual(harness.sentMessages.at(-1), {
    type: "petaldesk.popup.fill",
    entryId: "entry-b",
  });
  assert.equal(harness.status(), "选择要填充的账户：");
});

test("right-click and keyboard context-menu gestures never trigger filling", async () => {
  const harness = loadPopup();
  await flush();

  const fill = harness.buttonInCard(0, "填充 Example 的账户");
  fill.focus();
  const enter = harness.dispatchContextKey(0, "Enter", { target: fill });
  const space = harness.dispatchContextKey(0, " ", { target: fill });
  assert.equal(enter.prevented, false);
  assert.equal(space.prevented, false);
  assert.equal(harness.contextMenus().length, 0);

  harness.dispatchContextMenu(0, fill);
  assert.equal(harness.contextMenus().length, 1);
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.fill"),
    false,
  );
  harness.dispatchEscape();

  const keyboard = harness.dispatchContextKey(0, "ContextMenu", { target: fill });
  assert.equal(keyboard.prevented, true);
  assert.equal(keyboard.stopped, true);
  assert.equal(harness.contextMenus().length, 1);
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.fill"),
    false,
  );
});

test("right-click deletion opens a menu, then requires confirmation and refreshes", async () => {
  const harness = loadPopup();
  await flush();

  const fill = harness.buttonInCard(0, "填充 Example 的账户");
  fill.focus();
  const context = harness.dispatchContextMenu(0, fill);
  assert.equal(context.prevented, true);
  assert.equal(context.stopped, true);
  assert.equal(harness.contextMenus().length, 1);
  assert.equal(harness.dialogs().length, 0);
  assert.equal(harness.document.activeElement.getAttribute("aria-label"), "删除账户");
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.deleteEntry"),
    false,
  );

  harness.buttonInCard(0, "删除账户").click();
  assert.equal(harness.dialogs().length, 1);
  assert.equal(harness.contextMenus().length, 0);
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.deleteEntry"),
    false,
  );

  harness.buttonInCard(0, "取消").click();
  assert.equal(harness.dialogs().length, 0);
  assert.equal(harness.document.activeElement, fill);
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.deleteEntry"),
    false,
  );

  harness.dispatchContextMenu(0, fill);
  harness.buttonInCard(0, "删除账户").click();
  harness.buttonInCard(0, "删除").click();
  await flush();
  await flush();
  const removed = harness.sentMessages.find((message) => message.type === "petaldesk.popup.deleteEntry");
  assert.deepEqual(removed, { type: "petaldesk.popup.deleteEntry", entryId: "entry-a" });
  assert.equal(
    harness.sentMessages.filter((message) => message.type === "petaldesk.popup.getState").length,
    2,
  );
  assert.equal(harness.accountList.children.length, 1);
});

test("the popup refreshes only after the desktop delete acknowledgement", async () => {
  let resolveDelete;
  let deleteState;
  const acknowledgement = new Promise((resolve) => { resolveDelete = resolve; });
  const harness = loadPopup({
    onMessage(message, state) {
      if (message.type !== "petaldesk.popup.deleteEntry") return undefined;
      deleteState = state;
      return acknowledgement;
    },
  });
  await flush();

  harness.dispatchContextMenu(0);
  harness.buttonInCard(0, "删除账户").click();
  harness.buttonInCard(0, "删除").click();
  await flush();
  assert.equal(harness.accountList.children.length, 2);
  assert.equal(
    harness.sentMessages.filter((message) => message.type === "petaldesk.popup.getState").length,
    1,
  );

  deleteState.setTab({
    ...deleteState.getTab(),
    accounts: deleteState.getTab().accounts.filter((account) => account.entryId !== "entry-a"),
  });
  resolveDelete({ accepted: true });
  await flush();
  await flush();
  assert.equal(
    harness.sentMessages.filter((message) => message.type === "petaldesk.popup.getState").length,
    2,
  );
  assert.equal(harness.accountList.children.length, 1);
});

test("Escape closes the context menu or confirmation and restores the invoking focus", async () => {
  const harness = loadPopup();
  await flush();

  const fill = harness.buttonInCard(1, "填充 Example 的账户");
  fill.focus();
  harness.dispatchContextMenu(1, fill);
  assert.equal(harness.contextMenus().length, 1);
  assert.equal(harness.dispatchEscape(), true);
  assert.equal(harness.contextMenus().length, 0);
  assert.equal(harness.document.activeElement, fill);

  harness.dispatchContextKey(1, "F10", { shiftKey: true, target: fill });
  assert.equal(harness.contextMenus().length, 1);
  harness.buttonInCard(1, "删除账户").click();
  assert.equal(harness.dialogs().length, 1);
  assert.equal(harness.dispatchEscape(), true);
  assert.equal(harness.dialogs().length, 0);
  assert.equal(harness.document.activeElement, fill);
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.deleteEntry"),
    false,
  );
});

test("opening another account context menu closes the previous one", async () => {
  const harness = loadPopup();
  await flush();

  const fill = harness.buttonInCard(1, "填充 Example 的账户");
  harness.dispatchContextMenu(0);
  harness.dispatchContextMenu(1, fill);
  assert.equal(harness.contextMenus().length, 1);
  assert.equal(harness.dialogs().length, 0);
  assert.equal(harness.accountList.children[0].children.some((child) => child.className === "account-context-menu"), false);
  assert.equal(harness.accountList.children[1].children.some((child) => child.className === "account-context-menu"), true);
  harness.buttonInCard(1, "删除账户").click();
  harness.buttonInCard(1, "取消").click();
  assert.equal(harness.document.activeElement, fill);
});

test("clicking outside the account action closes its menu or confirmation", async () => {
  const harness = loadPopup();
  await flush();

  const fill = harness.buttonInCard(0, "填充 Example 的账户");
  harness.dispatchContextMenu(0, fill);
  harness.dispatchDocumentPointerDown(harness.elements.get("site-status"));
  assert.equal(harness.contextMenus().length, 0);
  assert.equal(harness.document.activeElement, fill);

  harness.dispatchContextMenu(0, fill);
  const deleteAction = harness.buttonInCard(0, "删除账户");
  harness.dispatchDocumentPointerDown(deleteAction);
  deleteAction.click();
  assert.equal(harness.dialogs().length, 1);
  harness.dispatchDocumentPointerDown(harness.elements.get("site-status"));
  assert.equal(harness.dialogs().length, 0);
  assert.equal(harness.document.activeElement, fill);
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.deleteEntry"),
    false,
  );
});

test("copy actions expose a disabled busy state until the request completes", async () => {
  let resolveCopy;
  const copyResult = new Promise((resolve) => { resolveCopy = resolve; });
  const harness = loadPopup({
    onMessage(message) {
      if (message.type === "petaldesk.popup.copySecret") return copyResult;
      return undefined;
    },
  });
  await flush();

  harness.dispatchContextMenu(0);
  const copy = harness.buttonInCard(0, "复制密码");
  copy.click();
  await Promise.resolve();
  assert.equal(harness.contextMenus().length, 0);
  assert.equal(harness.dialogs().length, 0);
  assert.equal(copy.disabled, true);
  assert.equal(copy.getAttribute("aria-busy"), "true");

  resolveCopy({ accepted: true });
  await flush();
  assert.equal(copy.disabled, false);
  assert.equal(copy.getAttribute("aria-busy"), null);
  assert.equal(harness.status(), "已复制密码");
});

test("an MFA copy error stays in popup status and does not disable password fill", async () => {
  const harness = loadPopup({
    onMessage(message) {
      if (message.type === "petaldesk.popup.copySecret" && message.field === "mfa") {
        return { ok: false, error: { message: "MFA 已锁定" } };
      }
      return undefined;
    },
  });
  await flush();

  harness.buttonInCard(1, "复制 MFA 验证码").click();
  await flush();
  assert.equal(harness.status(), "MFA 已锁定");

  harness.buttonInCard(1, "填充 Example 的账户").click();
  await flush();
  assert.equal(harness.sentMessages.at(-1).type, "petaldesk.popup.fill");
  assert.equal(harness.status(), "选择要填充的账户：");
});

test("popup CSS removes the delete slot and keeps context actions out of document flow", () => {
  const css = fs.readFileSync(path.join(__dirname, "..", "src", "popup", "popup.css"), "utf8");
  assert.match(css, /width:\s*360px/);
  assert.match(css, /grid-template-columns:\s*minmax\(0,\s*1fr\)\s+106px/);
  assert.match(css, /min-height:\s*64px/);
  assert.match(css, /\.account-context-menu\s*,\s*\.delete-confirm\s*\{[^}]*position:\s*absolute/s);
  assert.match(css, /\.delete-confirm\s*\{[^}]*position:\s*absolute/s);
  assert.equal(css.includes("grid-template-rows"), false);
  assert.equal(css.includes("delete-trigger"), false);
  assert.equal(css.includes("lucide-trash-2"), false);
});

test("the four locally packaged Lucide sources retain their package license marker", () => {
  const iconRoot = path.join(__dirname, "..", "assets", "icons", "lucide");
  for (const file of ["user-round.svg", "key-round.svg", "shield-check.svg", "trash-2.svg"]) {
    const source = fs.readFileSync(path.join(iconRoot, file), "utf8");
    assert.match(source, /@license lucide-static v1\.28\.0 - ISC/);
    assert.match(source, /viewBox="0 0 24 24"/);
  }
  assert.match(fs.readFileSync(path.join(iconRoot, "LICENSE"), "utf8"), /ISC License/);
});
