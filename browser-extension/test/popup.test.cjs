const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

function loadPopup({ tab } = {}) {
  const sentMessages = [];
  let currentTab = tab === undefined
    ? {
      accounts: [
        { entryId: "entry-a", siteName: "Example", username: "alice" },
        { entryId: "entry-b", siteName: "Example", username: "bob" },
      ],
      locked: false,
      origin: "https://example.test",
    }
    : tab;

  class FakeElement {
    constructor(tagName = "div") {
      this.tagName = String(tagName).toUpperCase();
      this.children = [];
      this.listeners = new Map();
      this.attributes = new Map();
      this.textContent = "";
      this.className = "";
      this.id = "";
      this.type = "";
    }

    get firstChild() {
      return this.children[0] || null;
    }

    append(...children) {
      this.children.push(...children);
    }

    appendChild(child) {
      this.append(child);
      return child;
    }

    removeChild(child) {
      const index = this.children.indexOf(child);
      if (index !== -1) this.children.splice(index, 1);
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

    click() {
      for (const listener of this.listeners.get("click") || []) {
        listener({ type: "click" });
      }
    }
  }

  const elements = new Map();
  for (const id of [
    "diagnostics-list",
    "site-origin",
    "site-status",
    "account-list",
    "open-manager",
    "extension-version",
  ]) {
    const element = new FakeElement(id.endsWith("-list") ? "ul" : "p");
    element.id = id;
    elements.set(id, element);
  }
  const document = {
    createElement(tagName) {
      return new FakeElement(tagName);
    },
    getElementById(id) {
      return elements.get(id) || null;
    },
  };
  const runtime = {
    sendMessage(message) {
      sentMessages.push(JSON.parse(JSON.stringify(message)));
      if (message.type === "petaldesk.popup.getState") {
        return Promise.resolve({
          diagnostics: { captureEnabled: true, extensionVersion: "0.7.2" },
          tab: currentTab,
        });
      }
      if (
        message.type === "petaldesk.popup.copySecret"
        || message.type === "petaldesk.popup.deleteEntry"
      ) {
        return Promise.resolve({ accepted: true });
      }
      return Promise.resolve({ ok: true });
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

  function collectButtons(element, found = []) {
    for (const child of element.children) {
      if (child.tagName === "BUTTON") found.push(child);
      collectButtons(child, found);
    }
    return found;
  }

  function collectLeafTexts(element, found = []) {
    for (const child of element.children) {
      if (child.children.length === 0 && child.textContent) found.push(child.textContent);
      collectLeafTexts(child, found);
    }
    return found;
  }

  return {
    elements,
    sentMessages,
    buttons() {
      return collectButtons(accountList).map((button) => button.textContent);
    },
    leafTexts() {
      return collectLeafTexts(accountList);
    },
    clickButton(label) {
      const button = collectButtons(accountList).find((candidate) => candidate.textContent === label);
      assert.ok(button, `the ${label} button should be rendered in the popup`);
      button.click();
    },
    status() {
      return elements.get("site-status").textContent;
    },
  };
}

test("the account menu copies the password through a background event", async () => {
  const harness = loadPopup();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(harness.buttons(), ["", "⋯", "", "⋯"]);
  harness.clickButton("⋯");
  assert.deepEqual(harness.buttons(), ["", "⋯", "复制账号", "复制密码", "删除账号", "", "⋯"]);
  harness.clickButton("复制密码");
  await new Promise((resolve) => setImmediate(resolve));
  const copy = harness.sentMessages.at(-1);
  assert.deepEqual(copy, {
    type: "petaldesk.popup.copySecret",
    entryId: "entry-a",
    field: "password",
  });
  assert.equal(harness.status(), "已复制密码。");
  // The menu closes after the action.
  assert.deepEqual(harness.buttons(), ["", "⋯", "", "⋯"]);
});

test("the account menu copies the username with the username field", async () => {
  const harness = loadPopup();
  await new Promise((resolve) => setImmediate(resolve));
  harness.clickButton("⋯");
  harness.clickButton("复制账号");
  await new Promise((resolve) => setImmediate(resolve));
  const copy = harness.sentMessages.at(-1);
  assert.deepEqual(copy, {
    type: "petaldesk.popup.copySecret",
    entryId: "entry-a",
    field: "username",
  });
  assert.equal(harness.status(), "已复制用户名。");
});

test("deleting an account requires an inline confirmation and then refreshes", async () => {
  const harness = loadPopup();
  await new Promise((resolve) => setImmediate(resolve));
  harness.clickButton("⋯");
  harness.clickButton("删除账号");
  assert.ok(harness.leafTexts().includes("确认删除？"));
  assert.deepEqual(harness.buttons(), ["", "⋯", "删除", "取消", "", "⋯"]);
  // Nothing is sent until the inline confirmation is used.
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.deleteEntry"),
    false,
  );
  harness.clickButton("取消");
  assert.deepEqual(harness.buttons(), ["", "⋯", "", "⋯"]);
  assert.equal(
    harness.sentMessages.some((message) => message.type === "petaldesk.popup.deleteEntry"),
    false,
  );
  harness.clickButton("⋯");
  harness.clickButton("删除账号");
  harness.clickButton("删除");
  await new Promise((resolve) => setImmediate(resolve));
  const removed = harness.sentMessages.find((message) => message.type === "petaldesk.popup.deleteEntry");
  assert.deepEqual(removed, { type: "petaldesk.popup.deleteEntry", entryId: "entry-a" });
  // The popup re-reads the extension state after a delete.
  const getStates = harness.sentMessages.filter((message) => message.type === "petaldesk.popup.getState");
  assert.equal(getStates.length, 2);
});

test("the menu toggle targets one account row at a time", async () => {
  const harness = loadPopup();
  await new Promise((resolve) => setImmediate(resolve));
  harness.clickButton("⋯");
  assert.deepEqual(harness.buttons(), ["", "⋯", "复制账号", "复制密码", "删除账号", "", "⋯"]);
  // Clicking the same trigger again closes the menu without any message.
  harness.clickButton("⋯");
  assert.deepEqual(harness.buttons(), ["", "⋯", "", "⋯"]);
  const sent = harness.sentMessages.map((message) => message.type);
  assert.deepEqual(sent, ["petaldesk.popup.getState"]);
});
