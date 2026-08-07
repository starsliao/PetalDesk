const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

function loadManager({
  ambiguous = false,
  fieldScenario = "login",
  templateMultiple = false,
  frame = null,
  otpScenario = null,
} = {}) {
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

    hasAttribute(name) {
      return name === "hidden" && this.hidden === true;
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

  const generic = Boolean(otpScenario) || fieldScenario !== "login" && fieldScenario !== "netease";
  const netease = fieldScenario === "netease";
  const username = new FakeInput(netease
    ? { name: "email", type: "email" }
    : generic
      ? fieldScenario === "search" ? { name: "q", type: "text" } : { autocomplete: "username", name: "account", type: "email" }
      : { name: "identifier", type: "email" });
  const password = new FakeInput(netease
    ? { autocomplete: "new-password", name: "password", type: "password" }
    : generic
      ? { autocomplete: "current-password", name: "currentPassword", type: "password" }
      : { autocomplete: templateMultiple ? "current-password" : "", name: "Passwd", type: "password" });
  const duplicatePassword = new FakeInput(netease
    ? { autocomplete: "new-password", name: "password", type: "password", hidden: true }
    : {
      autocomplete: templateMultiple ? "new-password" : "",
      name: "Passwd",
      type: "password",
    });
  const newPassword = new FakeInput({ autocomplete: "new-password", name: "newPassword", type: "password" });
  const otpInputs = (() => {
    const otp = (attributes) => new FakeInput({ type: "text", ...attributes });
    if (otpScenario === "single") {
      return [otp({ autocomplete: "one-time-code", inputmode: "numeric", maxLength: 6, name: "totp" })];
    }
    if (otpScenario === "authenticator-security-code") {
      return [otp({
        "aria-label": "Enter the 6-digit security code from your authenticator app",
        inputmode: "numeric",
        maxLength: 6,
        name: "security-code",
      })];
    }
    if (otpScenario === "authenticator-security-code-zh") {
      return [otp({
        "aria-label": "请输入身份验证器安全码",
        inputmode: "numeric",
        maxLength: 6,
        name: "security-code",
      })];
    }
    if (otpScenario === "seven" || otpScenario === "eight") {
      const digits = otpScenario === "seven" ? 7 : 8;
      return [otp({ autocomplete: "one-time-code", inputmode: "numeric", maxLength: digits, name: "mfa-code" })];
    }
    if (otpScenario === "segmented") {
      return Array.from({ length: 6 }, (_value, index) => otp({
        "aria-label": `OTP digit ${index + 1}`,
        inputmode: "numeric",
        maxLength: 1,
        name: `totp-${index + 1}`,
        type: "tel",
      }));
    }
    if (otpScenario === "ambiguous") {
      return [
        otp({ inputmode: "numeric", maxLength: 6, name: "verification-code" }),
        otp({ inputmode: "numeric", maxLength: 6, name: "auth-code" }),
      ];
    }
    if (otpScenario === "excluded") {
      return [
        otp({ autocomplete: "one-time-code", maxLength: 6, name: "sms-code" }),
        otp({ maxLength: 8, name: "recovery-code" }),
        otp({ maxLength: 6, name: "captcha-code" }),
        otp({ maxLength: 6, name: "card-cvv-code" }),
        otp({ "aria-label": "Card security code", inputmode: "numeric", maxLength: 6, name: "security-code" }),
        otp({ maxLength: 6, name: "postcode" }),
        otp({ maxLength: 6, name: "coupon-code" }),
        otp({ autocomplete: "one-time-code", hidden: true, maxLength: 6, name: "totp" }),
      ];
    }
    return [];
  })();
  const inputs = otpScenario
    ? otpInputs
    : fieldScenario === "empty"
    ? []
    : netease
      ? [username, password, duplicatePassword]
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
    otpInputs,
    replaceOtpInputs(attributesList) {
      const replacements = attributesList.map((attributes) => new FakeInput({ type: "text", ...attributes }));
      inputs.splice(0, inputs.length, ...replacements);
      return replacements;
    },
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

test("a trusted iframe confirms a broadcast offer and receives the secret", async () => {
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

test("the NetEase iframe template ignores a hidden password mirror", async () => {
  const harness = loadManager({
    fieldScenario: "netease",
    frame: { href: "https://dl.reg.163.com/login", top: false },
  });
  const offer = await harness.command("fillOffer", {
    direct: true,
    entryId: "entry-163-visible",
    offerId: "offer-163-visible",
    origin: "https://mail.163.com",
    sessionId: "session-163-visible",
    username: "alice@163.com",
  });
  assert.equal(offer.ok, true);
  assert.equal(offer.result.state, "confirmed");
  const credentials = {
    offerId: "offer-163-visible",
    origin: "https://mail.163.com",
    password: "secret-password",
    sessionId: "session-163-visible",
    username: "alice@163.com",
  };
  const result = await harness.command("fillSecret", credentials);
  assert.equal(result.ok, true);
  assert.equal(result.result.filledUsername, true);
  assert.equal(result.result.filledPassword, true);
  assert.equal(harness.inputs.username.value, "alice@163.com");
  assert.equal(harness.inputs.password.value, "secret-password");
  assert.equal(harness.inputs.duplicatePassword.value, "");
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

test("a cross-origin iframe cannot install capture listeners for another top site", async () => {
  const harness = loadManager({
    frame: { href: "https://attacker.github.io/phish", top: false },
  });
  const enabled = await harness.command("captureEnable", {
    insecureOrigins: [],
    topLevelOrigin: "https://victim.github.io",
  });
  assert.equal(enabled.ok, true);
  assert.equal(enabled.result.enabled, false);
  assert.equal(harness.documentListeners.has("click"), false);
  assert.equal(harness.documentListeners.has("submit"), false);
});

test("a trusted iframe without login fields silently ignores the offer", async () => {
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

test("a semantic login anchor click captures a password form", async () => {
  const harness = loadManager({
    fieldScenario: "netease",
    frame: { href: "https://dl.reg.163.com/login", top: false },
  });
  await harness.command("captureEnable", {});
  harness.inputs.username.value = "alice";
  harness.inputs.password.value = "secret-password";
  const loginAnchor = {
    id: "dologin",
    tagName: "A",
    textContent: "登录",
    type: "",
    getAttribute(name) {
      return name === "data-action" ? "dologin" : null;
    },
    closest(selector) {
      return selector === "form" ? null : this;
    },
  };
  harness.documentListeners.get("click")({ target: loginAnchor, isTrusted: false });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    harness.backgroundMessages.some(
      (message) => message.type === "petaldesk.password.capture-submitted",
    ),
    false,
    "a page-scripted click must not capture credentials",
  );
  harness.documentListeners.get("click")({ target: loginAnchor, isTrusted: true });
  await new Promise((resolve) => setImmediate(resolve));
  const submitted = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.capture-submitted",
  );
  assert.ok(submitted, "a semantic login anchor should report its candidate");
  assert.equal(submitted.candidate.origin, "https://dl.reg.163.com");
  assert.equal(submitted.candidate.username, "alice");
  assert.equal(submitted.candidate.password, "secret-password");
  const success = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.capture-success",
  );
  assert.ok(success, "a semantic login anchor should immediately promote its candidate");
  assert.equal(success.candidateId, submitted.candidate.candidateId);
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

test("a unique trusted TOTP field reports metadata only and fills without submitting", async () => {
  const harness = loadManager({ otpScenario: "single" });
  const armed = await harness.command("armSecondFactor", {
    expiresAt: Date.now() + 60_000,
    flowId: "flow-single",
  });
  assert.equal(armed.ok, true);
  const candidates = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.second-factor-candidates",
  );
  assert.deepEqual(Object.keys(candidates).sort(), ["confidence", "count", "digits", "type"]);
  assert.deepEqual(
    {
      confidence: candidates.confidence,
      count: candidates.count,
      digits: candidates.digits,
    },
    { confidence: "high", count: 1, digits: 6 },
  );
  assert.equal(JSON.stringify(candidates).includes("entry"), false);
  assert.equal(JSON.stringify(candidates).includes("123456"), false);
  await harness.command("bindSecondFactor", {
    challengeId: "challenge-single",
    expiresAt: Date.now() + 30_000,
    flowId: "flow-single",
  });

  const secret = { challengeId: "challenge-single", code: "123456", flowId: "flow-single" };
  const filled = await harness.command("provideSecondFactor", secret);
  assert.equal(filled.ok, true);
  assert.equal(filled.result.filled, true);
  assert.equal(filled.result.submitted, false);
  assert.equal(harness.otpInputs[0].value, "123456");
  assert.deepEqual(harness.otpInputs[0].events, ["input", "change"]);
  assert.equal(secret.code, "");
  assert.equal(harness.submitted(), false);
});

for (const scenario of ["authenticator-security-code", "authenticator-security-code-zh"]) {
  test(`${scenario} remains a trusted TOTP candidate`, async () => {
    const harness = loadManager({ otpScenario: scenario });
    await harness.command("armSecondFactor", {
      expiresAt: Date.now() + 60_000,
      flowId: `flow-${scenario}`,
    });
    const report = harness.backgroundMessages.find(
      (message) => message.type === "petaldesk.password.second-factor-candidates",
    );
    assert.deepEqual(
      { confidence: report.confidence, count: report.count, digits: report.digits },
      { confidence: "high", count: 1, digits: 6 },
    );
  });
}

test("a challenge never follows a same-shape replacement OTP element", async () => {
  const harness = loadManager({ otpScenario: "single" });
  await harness.command("armSecondFactor", {
    expiresAt: Date.now() + 60_000,
    flowId: "flow-replaced-field",
  });
  await harness.command("bindSecondFactor", {
    challengeId: "challenge-replaced-field",
    expiresAt: Date.now() + 30_000,
    flowId: "flow-replaced-field",
  });
  const replacements = harness.replaceOtpInputs([
    { autocomplete: "one-time-code", inputmode: "numeric", maxLength: 6, name: "totp" },
  ]);
  const secret = {
    challengeId: "challenge-replaced-field",
    code: "123456",
    flowId: "flow-replaced-field",
  };
  const rejected = await harness.command("provideSecondFactor", secret);
  assert.equal(rejected.ok, false);
  assert.match(rejected.error.message, /changed/i);
  assert.equal(replacements[0].value, "");
  assert.equal(secret.code, "");
});

test("six segmented TOTP fields receive one digit each and never synthesize submit events", async () => {
  const harness = loadManager({ otpScenario: "segmented" });
  await harness.command("armSecondFactor", {
    expiresAt: Date.now() + 60_000,
    flowId: "flow-segmented",
  });
  const report = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.second-factor-candidates",
  );
  assert.deepEqual(
    { confidence: report.confidence, count: report.count, digits: report.digits },
    { confidence: "high", count: 1, digits: 6 },
  );
  await harness.command("bindSecondFactor", {
    challengeId: "challenge-segmented",
    expiresAt: Date.now() + 30_000,
    flowId: "flow-segmented",
  });
  const filled = await harness.command("provideSecondFactor", {
    challengeId: "challenge-segmented",
    code: "654321",
    flowId: "flow-segmented",
  });
  assert.equal(filled.result.segmented, true);
  assert.equal(filled.result.fields, 6);
  assert.deepEqual(harness.otpInputs.map((input) => input.value), ["6", "5", "4", "3", "2", "1"]);
  assert.equal(harness.submitted(), false);
});

for (const [scenario, digits, code] of [["seven", 7, "1234567"], ["eight", 8, "12345678"]]) {
  test(`a ${digits}-digit TOTP field preserves its declared length`, async () => {
    const harness = loadManager({ otpScenario: scenario });
    await harness.command("armSecondFactor", {
      expiresAt: Date.now() + 60_000,
      flowId: `flow-${digits}`,
    });
    const report = harness.backgroundMessages.find(
      (message) => message.type === "petaldesk.password.second-factor-candidates",
    );
    assert.equal(report.digits, digits);
    await harness.command("bindSecondFactor", {
      challengeId: `challenge-${digits}`,
      expiresAt: Date.now() + 30_000,
      flowId: `flow-${digits}`,
    });
    const filled = await harness.command("provideSecondFactor", {
      challengeId: `challenge-${digits}`,
      code,
      flowId: `flow-${digits}`,
    });
    assert.equal(filled.result.digits, digits);
    assert.equal(harness.otpInputs[0].value, code);
  });
}

test("mixed candidate lengths prefer the strongest semantic TOTP field", async () => {
  const harness = loadManager({ otpScenario: "ambiguous" });
  harness.replaceOtpInputs([
    {
      autocomplete: "one-time-code",
      inputmode: "numeric",
      maxLength: 6,
      name: "verification-code",
    },
    {
      inputmode: "numeric",
      maxLength: 8,
      name: "authenticator-code",
    },
  ]);
  await harness.command("armSecondFactor", {
    expiresAt: Date.now() + 60_000,
    flowId: "flow-mixed-candidate-lengths",
  });
  await new Promise((resolve) => setImmediate(resolve));
  const report = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.second-factor-candidates",
  );
  assert.deepEqual(
    {
      confidence: report.confidence,
      count: report.count,
      digits: report.digits,
    },
    { confidence: "low", count: 2, digits: 8 },
  );
});

test("ambiguous MFA fields require the one-click prompt before a bound provide", async () => {
  const harness = loadManager({ otpScenario: "ambiguous" });
  await harness.command("armSecondFactor", {
    expiresAt: Date.now() + 60_000,
    flowId: "flow-ambiguous",
  });
  const report = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.second-factor-candidates",
  );
  assert.deepEqual(
    { confidence: report.confidence, count: report.count, digits: report.digits },
    { confidence: "low", count: 2, digits: 6 },
  );
  await harness.command("bindSecondFactor", {
    challengeId: "challenge-ambiguous",
    expiresAt: Date.now() + 30_000,
    flowId: "flow-ambiguous",
  });
  const offered = await harness.command("offerSecondFactor", {
    challengeId: "challenge-ambiguous",
    expiresAt: Date.now() + 30_000,
    flowId: "flow-ambiguous",
    requiresOriginConfirmation: false,
    topOrigin: "https://example.test",
  });
  assert.equal(offered.ok, true);
  assert.deepEqual(harness.overlayButtons(), ["一键填充", "取消"]);
  harness.clickButton("一键填充");
  await new Promise((resolve) => setImmediate(resolve));
  const confirmation = harness.backgroundMessages.at(-1);
  assert.deepEqual(
    {
      challengeId: confirmation.challengeId,
      flowId: confirmation.flowId,
      originConfirmed: confirmation.originConfirmed,
      type: confirmation.type,
    },
    {
      challengeId: "challenge-ambiguous",
      flowId: "flow-ambiguous",
      originConfirmed: false,
      type: "petaldesk.password.second-factor-confirm",
    },
  );
  const filled = await harness.command("provideSecondFactor", {
    challengeId: "challenge-ambiguous",
    code: "112233",
    flowId: "flow-ambiguous",
  });
  assert.equal(filled.result.filled, true);
  assert.equal(harness.otpInputs.filter((input) => input.value === "112233").length, 1);
});

test("first-time cross-origin MFA prompt names the exact HTTPS origin", async () => {
  const harness = loadManager({ otpScenario: "single" });
  await harness.command("armSecondFactor", {
    expiresAt: Date.now() + 60_000,
    flowId: "flow-cross-origin",
  });
  await harness.command("bindSecondFactor", {
    challengeId: "challenge-cross-origin",
    expiresAt: Date.now() + 30_000,
    flowId: "flow-cross-origin",
  });
  await harness.command("offerSecondFactor", {
    challengeId: "challenge-cross-origin",
    flowId: "flow-cross-origin",
    requiresOriginConfirmation: true,
    topOrigin: "https://login.partner.example",
  });
  assert.match(harness.overlayMessage(), /https:\/\/login\.partner\.example/);
  harness.clickButton("一键填充");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(harness.backgroundMessages.at(-1).originConfirmed, true);
});

test("cancelling the MFA prompt retires its challenge and reports no secret", async () => {
  const harness = loadManager({ otpScenario: "ambiguous" });
  await harness.command("armSecondFactor", {
    expiresAt: Date.now() + 60_000,
    flowId: "flow-cancel",
  });
  await harness.command("bindSecondFactor", {
    challengeId: "challenge-cancel",
    expiresAt: Date.now() + 30_000,
    flowId: "flow-cancel",
  });
  await harness.command("offerSecondFactor", {
    challengeId: "challenge-cancel",
    flowId: "flow-cancel",
    requiresOriginConfirmation: false,
    topOrigin: "https://example.test",
  });
  harness.clickButton("取消");
  await new Promise((resolve) => setImmediate(resolve));
  const cancelled = harness.backgroundMessages.at(-1);
  assert.deepEqual(cancelled, {
    type: "petaldesk.password.second-factor-cancel",
    flowId: "flow-cancel",
    challengeId: "challenge-cancel",
  });
  const secret = { challengeId: "challenge-cancel", code: "123456", flowId: "flow-cancel" };
  const rejected = await harness.command("provideSecondFactor", secret);
  assert.equal(rejected.ok, false);
  assert.equal(secret.code, "");
});

test("SMS, recovery, CAPTCHA, card, postcode, coupon, and hidden fields are excluded", async () => {
  const harness = loadManager({ otpScenario: "excluded" });
  await harness.command("armSecondFactor", {
    expiresAt: Date.now() + 60_000,
    flowId: "flow-excluded",
  });
  const report = harness.backgroundMessages.find(
    (message) => message.type === "petaldesk.password.second-factor-candidates",
  );
  assert.deepEqual(
    { confidence: report.confidence, count: report.count, digits: report.digits },
    { confidence: "low", count: 0, digits: 0 },
  );
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
