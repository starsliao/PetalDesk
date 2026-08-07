const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

function loadManager({ ambiguous = false, fieldScenario = "login", templateMultiple = false, frame = null } = {}) {
  const runtimeMessages = [];
  const backgroundMessages = [];
  const documentListeners = new Map();
  const windowListeners = new Map();
  const createdElements = [];
  let submitted = false;

  class FakeElement {
    constructor(tagName = "div") {
      this.tagName = tagName.toUpperCase();
      this.children = [];
      this.listeners = new Map();
      this.style = { cssText: "" };
      this.isConnected = false;
      this.textContent = "";
      this.className = "";
      this.id = "";
      this.type = "";
    }

    append(...children) {
      this.children.push(...children);
      children.forEach((child) => { child.isConnected = true; });
    }

    appendChild(child) {
      this.append(child);
      return child;
    }

    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) || [];
      listeners.push(listener);
      this.listeners.set(type, listeners);
    }

    dispatchEvent(event) {
      for (const listener of this.listeners.get(event.type) || []) listener(event);
      return true;
    }

    attachShadow() {
      const shadow = new FakeElement("shadow-root");
      this.shadowRootForTest = shadow;
      return shadow;
    }

    remove() {
      this.isConnected = false;
    }

    click() {
      this.dispatchEvent({ type: "click" });
    }
  }

  class FakeInput extends FakeElement {
    constructor(attributes) {
      super("input");
      Object.assign(this, attributes);
      this.value = "";
      this.events = [];
    }

    getAttribute(name) {
      return this[name] || null;
    }

    getClientRects() {
      return [{}];
    }

    matches(selector) {
      const match = selector.match(/^input\[(name|type|autocomplete)=["']([^"']+)["']\]$/);
      return Boolean(match && String(this[match[1]] || "") === match[2]);
    }

    dispatchEvent(event) {
      this.events.push(event.type);
      return super.dispatchEvent(event);
    }
  }

  const generic = fieldScenario !== "login";
  const username = new FakeInput(generic
    ? fieldScenario === "search" ? { name: "q", type: "text" } : { autocomplete: "username", name: "account", type: "email" }
    : { name: "identifier", type: "email" });
  const password = new FakeInput(generic
    ? { autocomplete: "current-password", name: "currentPassword", type: "password" }
    : { autocomplete: templateMultiple ? "current-password" : "", name: "Passwd", type: "password" });
  const duplicatePassword = new FakeInput({
    autocomplete: templateMultiple ? "new-password" : "",
    name: "Passwd",
    type: "password",
  });
  const newPassword = new FakeInput({ autocomplete: "new-password", name: "newPassword", type: "password" });
  const inputs = fieldScenario === "empty"
    ? []
    : fieldScenario === "multiple-passwords"
      ? [username, password, newPassword]
      : ambiguous || templateMultiple ? [username, password, duplicatePassword] : [username, password];
  const document = {
    body: new FakeElement("body"),
    createElement(tagName) {
      const element = new FakeElement(tagName);
      createdElements.push(element);
      return element;
    },
    documentElement: new FakeElement("html"),
    get visibilityState() {
      return "visible";
    },
    querySelectorAll(selector) {
      if (selector === "input") return inputs;
      return inputs.filter((input) => input.matches(selector));
    },
    addEventListener(type, listener) {
      documentListeners.set(type, listener);
    },
    removeEventListener(type) {
      documentListeners.delete(type);
    },
  };
  document.documentElement.appendChild(document.body);

  const runtime = {
    onMessage: {
      addListener(listener) {
        runtimeMessages.push(listener);
      },
    },
    sendMessage(message, callback) {
      backgroundMessages.push(JSON.parse(JSON.stringify(message)));
      const response = message.type === "petaldesk.password.template-recording-progress"
        ? { completed: message.field === "password" }
        : { captureEnabled: false };
      if (typeof callback === "function") callback(response);
      return Promise.resolve(response);
    },
  };
  const location = {
    href: generic ? "https://example.test/login" : "https://accounts.google.com/signin",
  };
  if (frame && frame.href) location.href = frame.href;
  if (frame && frame.ancestorOrigins) location.ancestorOrigins = frame.ancestorOrigins;
  const context = {
    URL,
    Event: class {
      constructor(type) {
        this.type = type;
      }
    },
     addEventListener(type, listener) {
       windowListeners.set(type, listener);
     },
    browser: { runtime },
    clearTimeout,
    console,
    crypto: { randomUUID: () => "manager-test" },
    document,
    location,
    setTimeout,
    top: null,
  };
  context.top = frame && frame.top === false ? { iframe: true } : context;
  context.globalThis = context;
  vm.createContext(context);
  for (const source of ["src/shared/password-templates.js", "src/content/password-manager.js"]) {
    vm.runInContext(
      fs.readFileSync(path.join(__dirname, "..", source), "utf8"),
      context,
      { filename: source },
    );
  }
  return {
    backgroundMessages,
    clickButton(label) {
      const button = createdElements.find((element) => element.tagName === "BUTTON" && element.textContent === label);
      assert.ok(button, `the ${label} button should be visible in the page overlay`);
      button.click();
    },
    clickFillButton() {
      const button = createdElements.find((element) => element.tagName === "BUTTON" && element.textContent === "填充");
      assert.ok(button, "the fill button should be visible in the page overlay");
      button.click();
    },
    overlayButtons() {
      return createdElements
        .filter((element) => element.tagName === "BUTTON")
        .map((element) => element.textContent);
    },
    overlayTitle() {
      const heading = createdElements.find((element) => element.tagName === "H2");
      return heading ? heading.textContent : "";
    },
    overlayMessage() {
      const paragraphs = createdElements.filter((element) => element.tagName === "P");
      return paragraphs.length ? paragraphs.at(-1).textContent : "";
    },
    command(command, payload) {
      return new Promise((resolve) => {
        runtimeMessages[0]({ type: "petaldesk.password.command", command, payload }, null, resolve);
      });
    },
    document,
    documentListeners,
    windowListeners,
    inputs: { duplicatePassword, newPassword, password, username },
    submitted: () => submitted,
    markSubmitted() {
      submitted = true;
    },
  };
}

test("password pages require an explicit overlay confirmation and never submit", async () => {
  const harness = loadManager();
  const offer = await harness.command("fillOffer", {
    entryId: "entry-1",
    offerId: "offer-1",
    origin: "https://accounts.google.com",
    sessionId: "session-1",
    username: "alice@example.com",
  });
  assert.equal(offer.ok, true);
  assert.equal(harness.inputs.username.value, "");
  assert.equal(harness.inputs.password.value, "");
  harness.clickFillButton();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(harness.backgroundMessages.at(-1).type, "petaldesk.password.fill-confirm");
  const credentials = {
    offerId: "offer-1",
    origin: "https://accounts.google.com",
    password: "secret-password",
    sessionId: "session-1",
    username: "alice@example.com",
  };
  const result = await harness.command("fillSecret", credentials);
  assert.equal(result.ok, true);
  assert.equal(harness.inputs.username.value, "alice@example.com");
  assert.equal(harness.inputs.password.value, "secret-password");
  assert.deepEqual(harness.inputs.password.events, ["input", "change"]);
  assert.equal(result.result.submitted, false);
  assert.equal(credentials.password, "");
  assert.equal(harness.submitted(), false);
});

test("direct fill offers confirm themselves and fill without a confirmation overlay", async () => {
  const harness = loadManager();
  const offer = await harness.command("fillOffer", {
    direct: true,
    entryId: "entry-direct",
    offerId: "offer-direct",
    origin: "https://accounts.google.com",
    sessionId: "session-direct",
    username: "alice@example.com",
  });
  assert.equal(offer.ok, true);
  assert.equal(offer.result.state, "confirmed");
  // No confirmation overlay is shown for a direct fill.
  assert.deepEqual(harness.overlayButtons(), []);
  // The content script confirms immediately on the user's behalf.
  const confirm = harness.backgroundMessages.at(-1);
  assert.equal(confirm.type, "petaldesk.password.fill-confirm");
  assert.equal(confirm.sessionId, "session-direct");
  assert.equal(confirm.offerId, "offer-direct");
  assert.equal(confirm.origin, "https://accounts.google.com");
  const credentials = {
    offerId: "offer-direct",
    origin: "https://accounts.google.com",
    password: "secret-password",
    sessionId: "session-direct",
    username: "alice@example.com",
  };
  const result = await harness.command("fillSecret", credentials);
  assert.equal(result.ok, true);
  assert.equal(result.result.submitted, false);
  assert.equal(harness.inputs.username.value, "alice@example.com");
  assert.equal(harness.inputs.password.value, "secret-password");
  assert.equal(credentials.password, "");
  // A lightweight notice replaces the confirmation overlay after filling.
  assert.equal(harness.overlayTitle(), "飞花密码管理器");
  assert.match(harness.overlayMessage(), /已填充 alice@example\.com，未自动提交/);
  assert.deepEqual(harness.overlayButtons(), []);
  assert.equal(harness.submitted(), false);
});

test("a submitted password form is reported as a success immediately", async () => {
  const harness = loadManager();
  const enabled = await harness.command("captureEnable", {});
  assert.equal(enabled.ok, true);
  assert.equal(enabled.result.enabled, true);
  harness.inputs.username.value = "alice";
  harness.inputs.password.value = "secret-password";
  harness.documentListeners.get("submit")({ target: harness.document });
  // A single microtask flush is enough: there is no post-submit settle delay.
  await new Promise((resolve) => setImmediate(resolve));
  const types = harness.backgroundMessages.map((message) => message.type);
  const submittedIndex = types.indexOf("petaldesk.password.capture-submitted");
  assert.notEqual(submittedIndex, -1);
  assert.equal(types[submittedIndex + 1], "petaldesk.password.capture-success");
  const submittedMessage = harness.backgroundMessages[submittedIndex];
  assert.equal(submittedMessage.candidate.username, "alice");
  assert.equal(submittedMessage.candidate.password, "secret-password");
  const successMessage = harness.backgroundMessages[submittedIndex + 1];
  assert.equal(successMessage.candidateId, submittedMessage.candidate.candidateId);
  assert.equal(successMessage.confidence, "high");
  assert.equal(successMessage.origin, "https://accounts.google.com");
  // Disabling capture clears the in-memory candidate TTL timer.
  await harness.command("captureDisable", {});
});

test("ambiguous login fields fail closed before credentials are written", async () => {
  const harness = loadManager({ ambiguous: true });
  await harness.command("fillOffer", {
    entryId: "entry-ambiguous",
    offerId: "offer-ambiguous",
    origin: "https://accounts.google.com",
    sessionId: "session-ambiguous",
    username: "alice@example.com",
  });
  harness.clickFillButton();
  await new Promise((resolve) => setImmediate(resolve));
  const credentials = {
    offerId: "offer-ambiguous",
    origin: "https://accounts.google.com",
    password: "secret-password",
    sessionId: "session-ambiguous",
    username: "alice@example.com",
  };
  const result = await harness.command("fillSecret", credentials);
  assert.equal(result.ok, false);
  assert.match(result.error.message, /ambiguous/i);
  assert.equal(harness.inputs.username.value, "");
  assert.equal(harness.inputs.password.value, "");
  assert.equal(harness.inputs.duplicatePassword.value, "");
  assert.equal(credentials.password, "");
  assert.equal(credentials.username, "");
});

async function attemptGenericFill(fieldScenario) {
  const harness = loadManager({ fieldScenario });
  await harness.command("fillOffer", {
    entryId: `entry-${fieldScenario}`,
    offerId: `offer-${fieldScenario}`,
    origin: "https://example.test",
    sessionId: `session-${fieldScenario}`,
    username: "alice@example.com",
  });
  harness.clickFillButton();
  await new Promise((resolve) => setImmediate(resolve));
  const credentials = {
    offerId: `offer-${fieldScenario}`,
    origin: "https://example.test",
    password: "secret-password",
    sessionId: `session-${fieldScenario}`,
    username: "alice@example.com",
  };
  const result = await harness.command("fillSecret", credentials);
  return { credentials, harness, result };
}

test("generic filling rejects a search input beside a password field", async () => {
  const { credentials, harness, result } = await attemptGenericFill("search");
  assert.equal(result.ok, false);
  assert.match(result.error.message, /ambiguous/i);
  assert.equal(harness.inputs.username.value, "");
  assert.equal(harness.inputs.password.value, "");
  assert.equal(credentials.password, "");
});

test("generic filling rejects multiple password fields even when their scores differ", async () => {
  const { credentials, harness, result } = await attemptGenericFill("multiple-passwords");
  assert.equal(result.ok, false);
  assert.match(result.error.message, /ambiguous/i);
  assert.equal(harness.inputs.password.value, "");
  assert.equal(harness.inputs.newPassword.value, "");
  assert.equal(credentials.password, "");
});

test("template filling rejects multiple fields matched by the same password selector", async () => {
  const harness = loadManager({ templateMultiple: true });
  await harness.command("fillOffer", {
    entryId: "entry-template-multiple",
    offerId: "offer-template-multiple",
    origin: "https://accounts.google.com",
    sessionId: "session-template-multiple",
    username: "alice@example.com",
  });
  harness.clickFillButton();
  await new Promise((resolve) => setImmediate(resolve));
  const credentials = {
    offerId: "offer-template-multiple",
    origin: "https://accounts.google.com",
    password: "secret-password",
    sessionId: "session-template-multiple",
    username: "alice@example.com",
  };
  const result = await harness.command("fillSecret", credentials);
  assert.equal(result.ok, false);
  assert.match(result.error.message, /ambiguous/i);
  assert.equal(harness.inputs.password.value, "");
  assert.equal(harness.inputs.duplicatePassword.value, "");
  assert.equal(credentials.password, "");
});

test("fill offers are silently ignored when the page has no login fields", async () => {
  const harness = loadManager({ fieldScenario: "empty" });
  const offer = await harness.command("fillOffer", {
    entryId: "entry-empty",
    offerId: "offer-empty",
    origin: "https://example.test",
    sessionId: "session-empty",
    username: "alice@example.com",
  });
  assert.equal(offer.ok, true);
  assert.equal(offer.result.ignored, true);
  assert.deepEqual(harness.overlayButtons(), []);
  // Without an active offer the secret request still fails closed.
  const credentials = {
    offerId: "offer-empty",
    origin: "https://example.test",
    password: "secret-password",
    sessionId: "session-empty",
    username: "alice@example.com",
  };
  const result = await harness.command("fillSecret", credentials);
  assert.equal(result.ok, false);
  assert.equal(credentials.password, "");
});

test("a same-site iframe confirms a broadcast offer and receives the secret", async () => {
  const harness = loadManager({
    frame: {
      ancestorOrigins: ["https://mail.163.com"],
      href: "https://dl.reg.163.com/login",
      top: false,
    },
  });
  const offer = await harness.command("fillOffer", {
    direct: true,
    entryId: "entry-163",
    offerId: "offer-163",
    origin: "https://mail.163.com",
    sessionId: "session-163",
    username: "alice@163.com",
  });
  assert.equal(offer.ok, true);
  assert.equal(offer.result.state, "confirmed");
  const confirm = harness.backgroundMessages.at(-1);
  assert.equal(confirm.type, "petaldesk.password.fill-confirm");
  assert.equal(confirm.origin, "https://mail.163.com");
  assert.equal(confirm.frameOrigin, "https://dl.reg.163.com");
  const credentials = {
    offerId: "offer-163",
    origin: "https://mail.163.com",
    password: "secret-password",
    sessionId: "session-163",
    username: "alice@163.com",
  };
  const result = await harness.command("fillSecret", credentials);
  assert.equal(result.ok, true);
  assert.equal(result.result.submitted, false);
  assert.equal(harness.inputs.username.value, "alice@163.com");
  assert.equal(harness.inputs.password.value, "secret-password");
  assert.equal(credentials.password, "");
});

test("a cross-site iframe silently ignores a broadcast fill offer", async () => {
  const harness = loadManager({
    frame: { href: "https://ads.example.net/banner", top: false },
  });
  const offer = await harness.command("fillOffer", {
    direct: true,
    entryId: "entry-163",
    offerId: "offer-xsite",
    origin: "https://mail.163.com",
    sessionId: "session-xsite",
    username: "alice@163.com",
  });
  assert.equal(offer.ok, true);
  assert.equal(offer.result.ignored, true);
  assert.equal(
    harness.backgroundMessages.some((message) => message.type === "petaldesk.password.fill-confirm"),
    false,
  );
  assert.deepEqual(harness.overlayButtons(), []);
  assert.equal(harness.inputs.password.value, "");
});

test("a same-site iframe without login fields silently ignores the offer", async () => {
  const harness = loadManager({
    fieldScenario: "empty",
    frame: { href: "https://dl.reg.163.com/frame", top: false },
  });
  const offer = await harness.command("fillOffer", {
    direct: true,
    entryId: "entry-163",
    offerId: "offer-no-fields",
    origin: "https://mail.163.com",
    sessionId: "session-no-fields",
    username: "alice@163.com",
  });
  assert.equal(offer.ok, true);
  assert.equal(offer.result.ignored, true);
  assert.equal(
    harness.backgroundMessages.some((message) => message.type === "petaldesk.password.fill-confirm"),
    false,
  );
  assert.deepEqual(harness.overlayButtons(), []);
});

test("an iframe capture reports the top-level origin plus its frame origin", async () => {
  const harness = loadManager({
    frame: {
      ancestorOrigins: ["https://mail.163.com"],
      href: "https://dl.reg.163.com/login",
      top: false,
    },
  });
  const enabled = await harness.command("captureEnable", {});
  assert.equal(enabled.result.enabled, true);
  harness.inputs.username.value = "alice";
  harness.inputs.password.value = "secret-password";
  harness.documentListeners.get("submit")({ target: harness.document });
  await new Promise((resolve) => setImmediate(resolve));
  const submitted = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.capture-submitted",
  );
  assert.ok(submitted, "the iframe should report its submitted candidate");
  assert.equal(submitted.candidate.origin, "https://mail.163.com");
  assert.equal(submitted.candidate.frameOrigin, "https://dl.reg.163.com");
  assert.equal(submitted.candidate.password, "secret-password");
  const success = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.capture-success",
  );
  assert.ok(success);
  assert.equal(success.origin, "https://mail.163.com");
  assert.equal(success.candidateId, submitted.candidate.candidateId);
  await harness.command("captureDisable", {});
});

test("an iframe capture falls back to its own origin without ancestorOrigins", async () => {
  const harness = loadManager({
    frame: { href: "https://dl.reg.163.com/login", top: false },
  });
  await harness.command("captureEnable", {});
  harness.inputs.username.value = "alice";
  harness.inputs.password.value = "secret-password";
  harness.documentListeners.get("submit")({ target: harness.document });
  await new Promise((resolve) => setImmediate(resolve));
  const submitted = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.capture-submitted",
  );
  assert.ok(submitted);
  assert.equal(submitted.candidate.origin, "https://dl.reg.163.com");
  assert.equal(submitted.candidate.frameOrigin, "https://dl.reg.163.com");
  await harness.command("captureDisable", {});
});

test("fill offers reject legacy template IDs instead of silently using generic fields", async () => {
  const harness = loadManager();
  const offer = await harness.command("fillOffer", {
    entryId: "entry-legacy",
    offerId: "offer-legacy",
    origin: "https://accounts.google.com",
    sessionId: "session-legacy",
    userTemplate: "legacy-template-id",
    username: "alice@example.com",
  });
  assert.equal(offer.ok, false);
  assert.match(offer.error.message, /template object/i);
});

test("template recording captures stable selectors without reading field values", async () => {
  const harness = loadManager();
  harness.inputs.username.value = "must-not-leave-page";
  harness.inputs.password.value = "must-not-leave-page-either";
  const started = await harness.command("templateRecordStart", {
    origin: "https://accounts.google.com",
    sessionId: "recording-1",
  });
  assert.equal(started.ok, true);
  const clickUsername = {
    target: harness.inputs.username,
    preventDefault() {},
    stopImmediatePropagation() {},
  };
  harness.documentListeners.get("click")(clickUsername);
  await new Promise((resolve) => setImmediate(resolve));
  const usernameMessage = harness.backgroundMessages.at(-1);
  assert.equal(usernameMessage.type, "petaldesk.password.template-recording-progress");
  assert.equal(usernameMessage.field, "username");
  assert.equal(usernameMessage.selector, 'input[name="identifier"]');
  assert.equal(Object.prototype.hasOwnProperty.call(usernameMessage, "value"), false);

  harness.documentListeners.get("click")({
    target: harness.inputs.password,
    preventDefault() {},
    stopImmediatePropagation() {},
  });
  await new Promise((resolve) => setImmediate(resolve));
  const passwordMessage = harness.backgroundMessages.at(-1);
  assert.equal(passwordMessage.field, "password");
  assert.equal(passwordMessage.selector, 'input[name="Passwd"]');
  assert.equal(JSON.stringify(passwordMessage).includes("must-not-leave-page"), false);
  assert.equal(harness.documentListeners.has("click"), false);
});

test("pagehide notifies the background bridge before local candidate cleanup", () => {
  const harness = loadManager();
  harness.windowListeners.get("pagehide")({ type: "pagehide" });
  assert.equal(harness.backgroundMessages.at(-1).type, "petaldesk.password.page-closed");
});

test("a new capture match with accounts offers save-as-new plus per-account updates", async () => {
  const harness = loadManager();
  const result = await harness.command("captureMatch", {
    accounts: [
      { entryId: "entry-a", siteName: "Example", username: "alice" },
      { entryId: "entry-b", siteName: "Example", username: "bob" },
    ],
    action: "new",
    candidateId: "new-candidate",
    confidence: "high",
    origin: "https://accounts.google.com",
    username: "alice",
  });
  assert.equal(result.ok, true);
  assert.equal(result.result.action, "new");
  assert.deepEqual(harness.overlayButtons(), ["保存为新账户", "更新 alice", "更新 bob", "忽略"]);
  harness.clickButton("保存为新账户");
  await new Promise((resolve) => setImmediate(resolve));
  const decision = harness.backgroundMessages.at(-1);
  assert.equal(decision.type, "petaldesk.password.save-decision");
  assert.equal(decision.action, "new");
  assert.equal(Object.prototype.hasOwnProperty.call(decision, "entryId"), false);
});

test("a per-account update button sends a bound replace decision", async () => {
  const harness = loadManager();
  await harness.command("captureMatch", {
    accounts: [
      { entryId: "entry-a", siteName: "Example", username: "alice" },
      { entryId: "entry-b", siteName: "Example", username: "bob" },
    ],
    action: "new",
    candidateId: "new-candidate",
    confidence: "high",
    origin: "https://accounts.google.com",
    username: "alice",
  });
  harness.clickButton("更新 bob");
  await new Promise((resolve) => setImmediate(resolve));
  const decision = harness.backgroundMessages.at(-1);
  assert.equal(decision.type, "petaldesk.password.save-decision");
  assert.equal(decision.action, "replace");
  assert.equal(decision.entryId, "entry-b");
});

test("a locked capture match shows an unlock notice with only a close button", async () => {
  const harness = loadManager();
  const result = await harness.command("captureMatch", {
    action: "locked",
    candidateId: "locked-candidate",
    origin: "https://accounts.google.com",
    username: "alice",
  });
  assert.equal(result.ok, true);
  assert.equal(result.result.action, "locked");
  assert.equal(harness.overlayTitle(), "飞花密码库已锁定");
  assert.deepEqual(harness.overlayButtons(), ["关闭"]);
});
