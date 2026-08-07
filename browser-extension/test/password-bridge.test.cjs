const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

function eventTarget() {
  const listeners = [];
  return {
    listeners,
    addListener(listener) {
      listeners.push(listener);
    },
  };
}

function loadBridge() {
  const events = [];
  const runtimeMessages = eventTarget();
  const tabActivations = eventTarget();
  const tabRemovals = eventTarget();
  const tabs = new Map();
  const tabMessages = [];
  let nextTabId = 40;
  let activeTabId = null;
  const actionUpdates = {
    badgeTexts: [],
  };
  const api = {
    browserFamily: "firefox",
    extensionVersion: "0.7.4",
    action: {
      async setBadgeText(value) {
        actionUpdates.badgeTexts.push(JSON.parse(JSON.stringify(value)));
      },
    },
    async createTab({ url }) {
      const tab = { id: ++nextTabId, url };
      tabs.set(tab.id, tab);
      return tab;
    },
    async getTab(tabId) {
      return tabs.get(tabId) || { id: tabId, url: "https://accounts.google.com/signin" };
    },
    onActivated(listener) {
      tabActivations.addListener(listener);
      return true;
    },
    onTabRemoved(listener) {
      tabRemovals.addListener(listener);
      return true;
    },
    async queryActiveTab() {
      return activeTabId == null ? null : tabs.get(activeTabId) || null;
    },
    async queryAllTabs() {
      return Array.from(tabs.values());
    },
    runtime: { id: "petaldesk-capture@petaldesk.app", onMessage: runtimeMessages },
    async sendTabMessage(tabId, message, options) {
      tabMessages.push({ tabId, message: JSON.parse(JSON.stringify(message)), options });
      const tab = tabs.get(tabId);
      assert.ok(tab, `tab ${tabId} should exist`);
      // Broadcast commands (fillOffer, fillCancel, captureEnable, ...) carry no
      // frameId; targeted commands must name a concrete frame.
      if (options != null) {
        assert.ok(Number.isInteger(options.frameId) && options.frameId >= 0);
      }
      if (message.command === "fillOffer") {
        assert.equal(Object.prototype.hasOwnProperty.call(message.payload, "password"), false);
      }
      if (message.command === "fillSecret") {
        assert.equal(message.payload.password, "secret-password");
      }
      return {
        ok: true,
        result: message.command === "fillSecret"
          ? { filledUsername: true, filledPassword: true, needsNextStep: false, submitted: false }
          : { state: message.command },
      };
    },
  };
  const context = {
    URL,
    clearTimeout,
    console,
    crypto: { randomUUID: () => "test-id" },
    setTimeout,
  };
  context.globalThis = context;
  vm.createContext(context);
  for (const source of ["src/shared/password-templates.js", "src/background/password-bridge.js"]) {
    vm.runInContext(
      fs.readFileSync(path.join(__dirname, "..", source), "utf8"),
      context,
      { filename: source },
    );
  }
  const bridge = context.PetalDeskPasswordBridge.createPasswordBridge({
    api,
    postToNative(message) {
      events.push(JSON.parse(JSON.stringify(message)));
    },
    protocolVersion: 1,
  });
  async function sendContent(message, sender) {
    const result = await new Promise((resolve) => {
      runtimeMessages.listeners[0](message, sender, resolve);
    });
    if (result && result.ok === false) {
      throw new Error(result.error && result.error.message || "Password event failed");
    }
    return result;
  }
  async function sendPopup(message, sender = { id: api.runtime.id }) {
    return new Promise((resolve) => {
      runtimeMessages.listeners[0](message, sender, resolve);
    });
  }
  return {
    actionUpdates,
    api,
    bridge,
    events,
    sendContent,
    sendPopup,
    setActiveTab(tabId) {
      activeTabId = tabId;
    },
    tabActivations,
    tabMessages,
    tabRemovals,
    tabs,
  };
}

test("the consent probe is a no-op now that authentication access is granted at install", async () => {
  const harness = loadBridge();
  assert.deepEqual(JSON.parse(JSON.stringify(await harness.bridge.route({ command: "password.requestConsent", payload: {} }))), {
    actionRequired: null,
    granted: true,
    userGestureRequired: false,
  });
  const status = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(status.authenticationConsent, true);
  assert.equal(status.consentArmed, false);
  assert.equal(status.consentActionRequired, null);
  assert.equal(status.captureEnabled, false);
});

test("fill sessions bind the new tab, origin, frame, and confirmation before receiving secrets", async () => {
  const harness = loadBridge();
  const opened = await harness.bridge.route({
    command: "password.open",
    payload: {
      entryId: "entry-1",
      origin: "https://accounts.google.com",
      sessionId: "session-1",
      url: "https://accounts.google.com/signin",
    },
  });
  const sender = {
    documentId: "document-1",
    frameId: 0,
    tab: { id: opened.tabId, url: "https://accounts.google.com/signin" },
    url: "https://accounts.google.com/signin",
  };
  const ready = await harness.sendContent(
    {
      type: "petaldesk.password.tab-ready",
      origin: "https://accounts.google.com",
      url: sender.url,
    },
    sender,
  );
  assert.equal(ready.captureEnabled, false);
  await harness.bridge.route({
    command: "password.offerFill",
    payload: {
      entryId: "entry-1",
      offerId: "offer-1",
      origin: "https://accounts.google.com",
      sessionId: "session-1",
      username: "alice@example.com",
    },
  });
  assert.equal(harness.tabMessages.at(-1).message.command, "fillOffer");
  assert.equal(harness.tabMessages.at(-1).message.payload.direct, true);
  await assert.rejects(
    harness.bridge.route({
      command: "password.provideCredentials",
      payload: {
        offerId: "offer-1",
        origin: "https://accounts.google.com",
        password: "secret-password",
        sessionId: "session-1",
      },
    }),
    /not confirmed/i,
  );
  const confirmed = await harness.sendContent(
    {
      type: "petaldesk.password.fill-confirm",
      offerId: "offer-1",
      origin: "https://accounts.google.com",
      sessionId: "session-1",
    },
    sender,
  );
  assert.equal(confirmed.confirmed, true);
  await assert.rejects(
    harness.sendContent(
      {
        type: "petaldesk.password.fill-confirm",
        offerId: "offer-1",
        origin: "https://evil.example",
        sessionId: "session-1",
      },
      sender,
    ),
    /does not match|origin/i,
  );
  const credentials = {
    offerId: "offer-1",
    origin: "https://accounts.google.com",
    password: "secret-password",
    sessionId: "session-1",
  };
  const result = await harness.bridge.route({ command: "password.provideCredentials", payload: credentials });
  assert.equal(result.filledPassword, true);
  assert.equal(credentials.password, "");
  const fillResult = harness.events.at(-1);
  assert.equal(fillResult.event, "fillResult");
  assert.equal(fillResult.payload.frameId, 0);
  assert.equal(fillResult.payload.frameOrigin, "https://accounts.google.com");
  assert.equal(fillResult.payload.origin, "https://accounts.google.com");
  assert.equal(Object.prototype.hasOwnProperty.call(harness.events.at(-1).payload, "password"), false);
});

test("offerFillDirect creates a ready session on the live tab and follows the confirm flow", async () => {
  const harness = loadBridge();
  harness.tabs.set(80, { id: 80, url: "https://example.test/login" });
  await assert.rejects(
    harness.bridge.route({
      command: "password.offerFillDirect",
      payload: {
        entryId: "entry-1",
        offerId: "offer-1",
        origin: "https://example.test",
        password: "secret-password",
        sessionId: "direct-secret",
        tabId: 80,
      },
    }),
    /cannot contain a password/i,
  );
  await assert.rejects(
    harness.bridge.route({
      command: "password.offerFillDirect",
      payload: {
        entryId: "entry-1",
        offerId: "offer-1",
        origin: "https://other.test",
        sessionId: "direct-1",
        tabId: 80,
      },
    }),
    /not on the requested origin/i,
  );
  const offered = await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      documentId: "document-80",
      entryId: "entry-1",
      frameId: 0,
      offerId: "offer-1",
      origin: "https://example.test",
      sessionId: "direct-1",
      tabId: 80,
      username: "alice",
    },
  });
  assert.equal(offered.state, "fillOffer");
  assert.equal(offered.tabId, 80);
  assert.equal(harness.tabMessages.at(-1).message.command, "fillOffer");
  assert.equal(harness.tabMessages.at(-1).message.payload.entryId, "entry-1");
  assert.equal(harness.tabMessages.at(-1).message.payload.direct, true);
  const sender = {
    documentId: "document-80",
    frameId: 0,
    tab: { id: 80, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  const confirmed = await harness.sendContent(
    {
      type: "petaldesk.password.fill-confirm",
      offerId: "offer-1",
      origin: "https://example.test",
      sessionId: "direct-1",
    },
    sender,
  );
  assert.equal(confirmed.confirmed, true);
  assert.equal(harness.events.at(-1).event, "fillConfirm");
  const credentials = {
    offerId: "offer-1",
    origin: "https://example.test",
    password: "secret-password",
    sessionId: "direct-1",
  };
  const filled = await harness.bridge.route({ command: "password.provideCredentials", payload: credentials });
  assert.equal(filled.filledPassword, true);
  assert.equal(credentials.password, "");
  assert.equal(harness.events.at(-1).event, "fillResult");
});

test("a direct fill confirmation arriving before the offer resolves is honored", async () => {
  const harness = loadBridge();
  harness.tabs.set(81, { id: 81, url: "https://example.test/login" });
  const sender = {
    documentId: "document-81",
    frameId: 0,
    tab: { id: 81, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  const originalSendTabMessage = harness.api.sendTabMessage;
  let confirmation = null;
  harness.api.sendTabMessage = async (tabId, message, options) => {
    if (message.command === "fillOffer") {
      // Direct fills confirm from the page while the offer is still in flight.
      confirmation = harness.sendContent(
        {
          type: "petaldesk.password.fill-confirm",
          offerId: message.payload.offerId,
          origin: message.payload.origin,
          sessionId: message.payload.sessionId,
        },
        sender,
      );
    }
    return originalSendTabMessage(tabId, message, options);
  };
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      documentId: "document-81",
      entryId: "entry-1",
      frameId: 0,
      offerId: "offer-1",
      origin: "https://example.test",
      sessionId: "direct-race",
      tabId: 81,
      username: "alice",
    },
  });
  assert.ok(confirmation, "the fill offer should reach the page");
  assert.equal((await confirmation).confirmed, true);
  const credentials = {
    offerId: "offer-1",
    origin: "https://example.test",
    password: "secret-password",
    sessionId: "direct-race",
  };
  const filled = await harness.bridge.route({ command: "password.provideCredentials", payload: credentials });
  assert.equal(filled.filledPassword, true);
  assert.equal(credentials.password, "");
  assert.equal(harness.events.at(-1).event, "fillResult");
});

test("updateBadge shows the account count and caches accounts for the popup", async () => {
  const harness = loadBridge();
  harness.tabs.set(55, { id: 55, url: "https://example.test/login" });
  const result = await harness.bridge.route({
    command: "password.updateBadge",
    payload: {
      accounts: [
        { entryId: "entry-a", siteName: "Example", username: "alice" },
        { entryId: "entry-b", siteName: "Example", username: "bob" },
      ],
      origin: "https://example.test",
      tabId: 55,
    },
  });
  assert.deepEqual(JSON.parse(JSON.stringify(result)), { applied: true, tabId: 55 });
  assert.deepEqual(harness.actionUpdates.badgeTexts.at(-1), { tabId: 55, text: "2" });
  harness.setActiveTab(55);
  const state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.tab.origin, "https://example.test");
  assert.equal(state.tab.locked, false);
  assert.deepEqual(state.tab.accounts.map((account) => account.entryId), ["entry-a", "entry-b"]);
});

test("updateBadge hides the count while the vault is locked", async () => {
  const harness = loadBridge();
  harness.tabs.set(56, { id: 56, url: "https://example.test/login" });
  await harness.bridge.route({
    command: "password.updateBadge",
    payload: {
      accounts: [{ entryId: "entry-a", siteName: "Example", username: "alice" }],
      locked: true,
      origin: "https://example.test",
      tabId: 56,
    },
  });
  assert.deepEqual(harness.actionUpdates.badgeTexts.at(-1), { tabId: 56, text: "" });
  harness.setActiveTab(56);
  const state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.tab.locked, true);
});

test("updateBadge trims and validates accounts, and tab removal clears the cache", async () => {
  const harness = loadBridge();
  harness.tabs.set(57, { id: 57, url: "https://example.test/login" });
  const accounts = Array.from({ length: 17 }, (_value, index) => ({
    entryId: `entry-${index}`,
    siteName: "Example",
    username: `user-${index}`,
  }));
  accounts.push({ entryId: "", siteName: "Example", username: "invalid" });
  await harness.bridge.route({
    command: "password.updateBadge",
    payload: { accounts, origin: "https://example.test", tabId: 57 },
  });
  assert.deepEqual(harness.actionUpdates.badgeTexts.at(-1), { tabId: 57, text: "16" });
  harness.setActiveTab(57);
  let state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.tab.accounts.length, 16);
  harness.tabRemovals.listeners[0](57);
  state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.tab.accounts.length, 0);
  assert.equal(state.tab.locked, false);
});

test("tab-ready reports the active origin and tab activation replays the live url", async () => {
  const harness = loadBridge();
  harness.tabs.set(70, { id: 70, url: "https://example.test/login" });
  const sender = {
    documentId: "document-70",
    frameId: 0,
    tab: { id: 70, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  await harness.sendContent(
    { type: "petaldesk.password.tab-ready", origin: "https://example.test" },
    sender,
  );
  assert.equal(harness.events.at(-1).event, "originActive");
  assert.deepEqual(harness.events.at(-1).payload, { origin: "https://example.test", tabId: 70 });
  // The tab navigated while the desktop channel was down: the cached origin is
  // stale, so activation must report the live tab url instead.
  harness.tabs.set(70, { id: 70, url: "https://new-origin.test/home" });
  harness.tabActivations.listeners[0]({ tabId: 70, windowId: 1 });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(harness.events.at(-1).event, "originActive");
  assert.deepEqual(harness.events.at(-1).payload, { origin: "https://new-origin.test", tabId: 70 });
  // A non-web tab clears the badge and tells the desktop to drop its tracking.
  harness.tabs.set(71, { id: 71, url: "about:newtab" });
  harness.tabActivations.listeners[0]({ tabId: 71, windowId: 1 });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(harness.events.at(-1).payload, { origin: "", tabId: 71 });
  assert.deepEqual(harness.actionUpdates.badgeTexts.at(-1), { tabId: 71, text: "" });
});

test("a navigation to a new origin clears the cached badge accounts", async () => {
  const harness = loadBridge();
  harness.tabs.set(60, { id: 60, url: "https://example.test/login" });
  await harness.bridge.route({
    command: "password.updateBadge",
    payload: {
      accounts: [{ entryId: "entry-a", siteName: "Example", username: "alice" }],
      origin: "https://example.test",
      tabId: 60,
    },
  });
  assert.deepEqual(harness.actionUpdates.badgeTexts.at(-1), { tabId: 60, text: "1" });
  const sender = {
    documentId: "document-60",
    frameId: 0,
    tab: { id: 60, url: "https://other.test/login" },
    url: "https://other.test/login",
  };
  harness.tabs.set(60, { id: 60, url: "https://other.test/login" });
  await harness.sendContent(
    { type: "petaldesk.password.tab-ready", origin: "https://other.test" },
    sender,
  );
  assert.equal(harness.events.at(-1).event, "originActive");
  assert.deepEqual(harness.events.at(-1).payload, { origin: "https://other.test", tabId: 60 });
  assert.deepEqual(harness.actionUpdates.badgeTexts.at(-1), { tabId: 60, text: "" });
  harness.setActiveTab(60);
  const state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.tab.origin, "https://other.test");
  assert.equal(state.tab.accounts.length, 0);
});

test("secretConnected replays the active tab origin and updates diagnostics", async () => {
  const harness = loadBridge();
  harness.tabs.set(72, { id: 72, url: "https://example.test/login" });
  harness.setActiveTab(72);
  await harness.bridge.onSecretConnected();
  assert.equal(harness.events.at(-1).event, "originActive");
  assert.deepEqual(harness.events.at(-1).payload, { origin: "https://example.test", tabId: 72 });
  let state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.diagnostics.secretConnected, true);
  harness.bridge.disconnect();
  state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.diagnostics.secretConnected, false);
});

test("popup state reports diagnostics recorded at the command boundary", async () => {
  const harness = loadBridge();
  harness.bridge.setNativeConnected(true);
  await harness.bridge.route({ command: "password.getStatus", payload: {} });
  let state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.diagnostics.nativeConnected, true);
  assert.equal(state.diagnostics.extensionVersion, "0.7.4");
  assert.equal(state.diagnostics.lastCommandOk, true);
  assert.equal(state.diagnostics.lastCommandErrorCode, null);
  assert.equal(typeof state.diagnostics.lastCommandAt, "number");
  await assert.rejects(harness.bridge.route({ command: "password.bogus", payload: {} }));
  state = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(state.diagnostics.lastCommandOk, false);
  assert.equal(state.diagnostics.lastCommandErrorCode, "PASSWORD_COMMAND_UNSUPPORTED");
});

test("popup messages reject content-script and foreign senders", async () => {
  const harness = loadBridge();
  const foreign = await harness.sendPopup(
    { type: "petaldesk.popup.getState" },
    { id: "other-extension@example.test" },
  );
  assert.equal(foreign.ok, false);
  assert.equal(foreign.error.code, "PASSWORD_TARGET_INVALID");
  const forged = await harness.sendPopup(
    { type: "petaldesk.popup.getState" },
    {
      documentId: "forged-document",
      frameId: 0,
      id: "petaldesk-capture@petaldesk.app",
      tab: { id: 1, url: "https://example.test/login" },
      url: "https://example.test/login",
    },
  );
  assert.equal(forged.ok, false);
  assert.equal(forged.error.code, "PASSWORD_TARGET_INVALID");
  const legitimate = await harness.sendPopup({ type: "petaldesk.popup.getState" });
  assert.equal(legitimate.diagnostics.nativeConnected, false);
  assert.deepEqual(JSON.parse(JSON.stringify(legitimate.tab)), { accounts: [], locked: false, origin: "" });
});

test("popup fill validates the cached account and posts a fill request", async () => {
  const harness = loadBridge();
  harness.tabs.set(85, { id: 85, url: "https://example.test/login" });
  harness.setActiveTab(85);
  const sender = {
    documentId: "document-85",
    frameId: 0,
    tab: { id: 85, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  await harness.sendContent(
    { type: "petaldesk.password.tab-ready", origin: "https://example.test" },
    sender,
  );
  await harness.bridge.route({
    command: "password.updateBadge",
    payload: {
      accounts: [{ entryId: "entry-a", siteName: "Example", username: "alice" }],
      origin: "https://example.test",
      tabId: 85,
    },
  });
  const denied = await harness.sendPopup({ type: "petaldesk.popup.fill", entryId: "entry-unknown" });
  assert.equal(denied.ok, false);
  assert.equal(denied.error.code, "PASSWORD_TARGET_MISMATCH");
  const accepted = await harness.sendPopup({ type: "petaldesk.popup.fill", entryId: "entry-a" });
  assert.equal(accepted.accepted, true);
  assert.equal(harness.events.at(-1).event, "fillRequest");
  assert.deepEqual(harness.events.at(-1).payload, {
    documentId: "document-85",
    entryId: "entry-a",
    origin: "https://example.test",
    tabId: 85,
  });
  const opened = await harness.sendPopup({ type: "petaldesk.popup.openManager" });
  assert.equal(opened.accepted, true);
  assert.equal(harness.events.at(-1).event, "openPasswordManager");
  assert.deepEqual(harness.events.at(-1).payload, {});
});

test("popup copy and delete validate the cached account and post bare events", async () => {
  const harness = loadBridge();
  harness.tabs.set(86, { id: 86, url: "https://example.test/login" });
  harness.setActiveTab(86);
  await harness.bridge.route({
    command: "password.updateBadge",
    payload: {
      accounts: [
        { entryId: "entry-a", siteName: "Example", username: "alice" },
        { entryId: "entry-b", siteName: "Example", username: "bob" },
      ],
      origin: "https://example.test",
      tabId: 86,
    },
  });
  const badField = await harness.sendPopup({ type: "petaldesk.popup.copySecret", entryId: "entry-a", field: "totp" });
  assert.equal(badField.ok, false);
  assert.equal(badField.error.code, "PASSWORD_PROTOCOL_INVALID");
  const unknown = await harness.sendPopup({ type: "petaldesk.popup.copySecret", entryId: "entry-unknown", field: "password" });
  assert.equal(unknown.ok, false);
  assert.equal(unknown.error.code, "PASSWORD_TARGET_MISMATCH");
  const copied = await harness.sendPopup({ type: "petaldesk.popup.copySecret", entryId: "entry-a", field: "password" });
  assert.equal(copied.accepted, true);
  assert.equal(harness.events.at(-1).event, "copySecret");
  assert.deepEqual(harness.events.at(-1).payload, { entryId: "entry-a", field: "password" });
  const copiedUsername = await harness.sendPopup({ type: "petaldesk.popup.copySecret", entryId: "entry-b", field: "username" });
  assert.equal(copiedUsername.accepted, true);
  assert.deepEqual(harness.events.at(-1).payload, { entryId: "entry-b", field: "username" });
  const deleted = await harness.sendPopup({ type: "petaldesk.popup.deleteEntry", entryId: "entry-a" });
  assert.equal(deleted.accepted, true);
  assert.equal(harness.events.at(-1).event, "deleteEntry");
  // The delete event carries only the entry ID: no site or account metadata.
  assert.deepEqual(harness.events.at(-1).payload, { entryId: "entry-a" });
  // A locked vault rejects copy and delete for cached accounts.
  await harness.bridge.route({
    command: "password.updateBadge",
    payload: {
      accounts: [{ entryId: "entry-a", siteName: "Example", username: "alice" }],
      locked: true,
      origin: "https://example.test",
      tabId: 86,
    },
  });
  const lockedCopy = await harness.sendPopup({ type: "petaldesk.popup.copySecret", entryId: "entry-a", field: "password" });
  assert.equal(lockedCopy.ok, false);
  assert.equal(lockedCopy.error.code, "PASSWORD_TARGET_MISMATCH");
  const lockedDelete = await harness.sendPopup({ type: "petaldesk.popup.deleteEntry", entryId: "entry-a" });
  assert.equal(lockedDelete.ok, false);
  assert.equal(lockedDelete.error.code, "PASSWORD_TARGET_MISMATCH");
});

test("popup copy and delete reject content-script and foreign senders", async () => {
  const harness = loadBridge();
  harness.tabs.set(87, { id: 87, url: "https://example.test/login" });
  harness.setActiveTab(87);
  await harness.bridge.route({
    command: "password.updateBadge",
    payload: {
      accounts: [{ entryId: "entry-a", siteName: "Example", username: "alice" }],
      origin: "https://example.test",
      tabId: 87,
    },
  });
  const forgedCopy = await harness.sendPopup(
    { type: "petaldesk.popup.copySecret", entryId: "entry-a", field: "password" },
    {
      documentId: "forged-document",
      frameId: 0,
      id: "petaldesk-capture@petaldesk.app",
      tab: { id: 87, url: "https://example.test/login" },
      url: "https://example.test/login",
    },
  );
  assert.equal(forgedCopy.ok, false);
  assert.equal(forgedCopy.error.code, "PASSWORD_TARGET_INVALID");
  const foreignDelete = await harness.sendPopup(
    { type: "petaldesk.popup.deleteEntry", entryId: "entry-a" },
    { id: "other-extension@example.test" },
  );
  assert.equal(foreignDelete.ok, false);
  assert.equal(foreignDelete.error.code, "PASSWORD_TARGET_INVALID");
  assert.equal(
    harness.events.some((event) => event.event === "copySecret" || event.event === "deleteEntry"),
    false,
  );
});

test("capture candidates remain in memory until a bound save decision and then clear", async () => {
  const harness = loadBridge();
  await harness.bridge.route({
    command: "password.setCaptureEnabled",
    payload: { enabled: true },
  });
  const sender = {
    documentId: "document-2",
    frameId: 0,
    tab: { id: 99, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  harness.tabs.set(99, sender.tab);
  const candidate = {
    candidateId: "candidate-1",
    confidence: "high",
    origin: "https://example.test",
    password: "candidate-secret",
    username: "alice",
  };
  const accepted = await harness.sendContent(
    { type: "petaldesk.password.capture-submitted", candidate },
    sender,
  );
  assert.equal(accepted.accepted, true);
  assert.equal(candidate.password, "");
  const promoted = await harness.sendContent(
    {
      type: "petaldesk.password.capture-success",
      candidateId: "candidate-1",
      confidence: "high",
      origin: "https://example.test",
    },
    sender,
  );
  assert.equal(promoted.promoted, true);
  const nativeCandidate = harness.events.at(-1);
  assert.equal(nativeCandidate.event, "captureCandidate");
  assert.equal(nativeCandidate.payload.password, "candidate-secret");
  const before = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(before.pendingCandidates, 1);
  await harness.bridge.route({
    command: "password.captureMatch",
    payload: { action: "update", candidateId: "candidate-1" },
  });
  const decision = await harness.sendContent(
    { type: "petaldesk.password.save-decision", action: "update", candidateId: "candidate-1" },
    sender,
  );
  assert.equal(decision.accepted, true);
  assert.equal(harness.events.at(-1).event, "saveDecision");
  const pending = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(pending.pendingCandidates, 1);
  await harness.bridge.route({
    command: "password.saveResult",
    payload: { action: "update", candidateId: "candidate-1", success: true, entryId: "entry-1" },
  });
  const after = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(after.pendingCandidates, 0);
});

test("a new match with accounts allows replacing one of the offered accounts", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const sender = {
    documentId: "document-90",
    frameId: 0,
    tab: { id: 90, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  harness.tabs.set(90, sender.tab);
  await harness.sendContent(
    {
      type: "petaldesk.password.capture-submitted",
      candidate: {
        candidateId: "candidate-new",
        origin: "https://example.test",
        password: "new-secret",
        username: "alice",
      },
    },
    sender,
  );
  await harness.sendContent(
    {
      type: "petaldesk.password.capture-success",
      candidateId: "candidate-new",
      confidence: "high",
      origin: "https://example.test",
    },
    sender,
  );
  await harness.bridge.route({
    command: "password.captureMatch",
    payload: {
      accounts: [{ entryId: "entry-a", siteName: "Example", username: "alice" }],
      action: "new",
      candidateId: "candidate-new",
    },
  });
  assert.equal(harness.tabMessages.at(-1).message.command, "captureMatch");
  assert.equal(harness.tabMessages.at(-1).message.payload.action, "new");
  assert.equal(harness.tabMessages.at(-1).message.payload.accounts.length, 1);
  const replace = await harness.sendContent(
    {
      type: "petaldesk.password.save-decision",
      action: "replace",
      candidateId: "candidate-new",
      entryId: "entry-a",
    },
    sender,
  );
  assert.equal(replace.accepted, true);
  assert.equal(harness.events.at(-1).event, "saveDecision");
  assert.equal(harness.events.at(-1).payload.action, "replace");
  assert.equal(harness.events.at(-1).payload.entryId, "entry-a");
  await harness.bridge.route({
    command: "password.saveResult",
    payload: { action: "replace", candidateId: "candidate-new", entryId: "entry-a", success: true },
  });
  assert.equal((await harness.bridge.route({ command: "password.getStatus", payload: {} })).pendingCandidates, 0);
});

test("a locked capture match clears the candidate and notifies the page", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const sender = {
    documentId: "document-91",
    frameId: 0,
    tab: { id: 91, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  harness.tabs.set(91, sender.tab);
  await harness.sendContent(
    {
      type: "petaldesk.password.capture-submitted",
      candidate: {
        candidateId: "candidate-locked",
        origin: "https://example.test",
        password: "locked-secret",
        username: "alice",
      },
    },
    sender,
  );
  await harness.sendContent(
    {
      type: "petaldesk.password.capture-success",
      candidateId: "candidate-locked",
      confidence: "high",
      origin: "https://example.test",
    },
    sender,
  );
  const matched = await harness.bridge.route({
    command: "password.captureMatch",
    payload: { action: "locked", candidateId: "candidate-locked" },
  });
  assert.equal(matched.action, "locked");
  assert.equal(harness.tabMessages.at(-1).message.command, "captureMatch");
  assert.equal(harness.tabMessages.at(-1).message.payload.action, "locked");
  assert.equal((await harness.bridge.route({ command: "password.getStatus", payload: {} })).pendingCandidates, 0);
  await assert.rejects(
    harness.sendContent(
      { type: "petaldesk.password.save-decision", action: "new", candidateId: "candidate-locked" },
      sender,
    ),
    /expired/i,
  );
});

test("pagehide and tab removal clear candidates from the background bridge", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const sender = {
    documentId: "closed-document",
    frameId: 0,
    tab: { id: 101, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  harness.tabs.set(101, sender.tab);
  await harness.sendContent({
    type: "petaldesk.password.capture-submitted",
    candidate: {
      candidateId: "closed-candidate",
      origin: "https://example.test",
      password: "closed-secret",
      username: "alice",
    },
  }, sender);
  assert.equal((await harness.bridge.route({ command: "password.getStatus", payload: {} })).pendingCandidates, 1);
  await harness.sendContent({ type: "petaldesk.password.page-closed" }, sender);
  assert.equal((await harness.bridge.route({ command: "password.getStatus", payload: {} })).pendingCandidates, 0);
  assert.equal(harness.events.at(-1).event, "pageClosed");

  await harness.sendContent({
    type: "petaldesk.password.capture-submitted",
    candidate: {
      candidateId: "removed-candidate",
      origin: "https://example.test",
      password: "removed-secret",
      username: "alice",
    },
  }, sender);
  assert.equal((await harness.bridge.route({ command: "password.getStatus", payload: {} })).pendingCandidates, 1);
  harness.tabRemovals.listeners[0](101);
  assert.equal((await harness.bridge.route({ command: "password.getStatus", payload: {} })).pendingCandidates, 0);
});

test("disabling login capture clears pending extension-side candidates", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const sender = {
    documentId: "disable-document",
    frameId: 0,
    tab: { id: 96, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  harness.tabs.set(96, sender.tab);
  const candidate = {
    candidateId: "candidate-disable",
    confidence: "high",
    origin: "https://example.test",
    password: "candidate-secret",
    username: "alice",
  };
  await harness.sendContent(
    { type: "petaldesk.password.capture-submitted", candidate },
    sender,
  );
  const before = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(before.pendingCandidates, 1);

  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: false } });
  const after = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(after.captureEnabled, false);
  assert.equal(after.pendingCandidates, 0);
});

test("two-step capture carries a short-lived username across same-origin navigation", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const firstSender = {
    documentId: "username-document",
    frameId: 0,
    tab: { id: 98, url: "https://accounts.google.com/signin" },
    url: "https://accounts.google.com/signin",
  };
  harness.tabs.set(98, firstSender.tab);
  const usernameStage = {
    type: "petaldesk.password.capture-username-stage",
    origin: "https://accounts.google.com",
    username: "alice@example.com",
  };
  const staged = await harness.sendContent(usernameStage, firstSender);
  assert.equal(staged.accepted, true);
  assert.equal(usernameStage.username, "");

  const passwordSender = {
    ...firstSender,
    documentId: "password-document",
    url: "https://accounts.google.com/challenge/pwd",
  };
  const candidate = {
    candidateId: "two-step-candidate",
    origin: "https://accounts.google.com",
    password: "two-step-secret",
    username: "",
  };
  await harness.sendContent(
    { type: "petaldesk.password.capture-submitted", candidate },
    passwordSender,
  );
  await harness.sendContent(
    {
      type: "petaldesk.password.capture-success",
      candidateId: "two-step-candidate",
      confidence: "high",
      origin: "https://accounts.google.com",
    },
    passwordSender,
  );
  const nativeCandidate = harness.events.at(-1);
  assert.equal(nativeCandidate.event, "captureCandidate");
  assert.equal(nativeCandidate.payload.username, "alice@example.com");
  assert.equal(nativeCandidate.payload.password, "two-step-secret");
});

test("password-change capture without a known username is forwarded for account selection", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const sender = {
    documentId: "password-change-document",
    frameId: 0,
    tab: { id: 97, url: "https://example.test/change-password" },
    url: "https://example.test/change-password",
  };
  harness.tabs.set(97, sender.tab);
  const candidate = {
    candidateId: "password-change-candidate",
    origin: "https://example.test",
    password: "new-secret",
    username: "",
  };
  const accepted = await harness.sendContent(
    { type: "petaldesk.password.capture-submitted", candidate },
    sender,
  );
  assert.equal(accepted.accepted, true);
  await harness.sendContent(
    {
      type: "petaldesk.password.capture-success",
      candidateId: candidate.candidateId,
      confidence: "high",
      origin: candidate.origin,
    },
    sender,
  );
  assert.equal(harness.events.at(-1).event, "captureCandidate");
  assert.equal(harness.events.at(-1).payload.username, "");
  assert.equal(candidate.password, "");
  const status = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(status.pendingCandidates, 1);
});

test("account selection uses an allowed entry ID and keeps failed saves retryable", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const sender = {
    documentId: "account-select-document",
    frameId: 0,
    tab: { id: 95, url: "https://example.test/change-password" },
    url: "https://example.test/change-password",
  };
  harness.tabs.set(95, sender.tab);
  const candidate = {
    candidateId: "account-select-candidate",
    origin: "https://example.test",
    password: "new-secret",
    username: "",
  };
  await harness.sendContent({ type: "petaldesk.password.capture-submitted", candidate }, sender);
  await harness.sendContent({
    type: "petaldesk.password.capture-success",
    candidateId: candidate.candidateId,
    confidence: "high",
    origin: candidate.origin,
  }, sender);
  await harness.bridge.route({
    command: "password.captureMatch",
    payload: {
      action: "select",
      accounts: [
        { entryId: "entry-a", siteName: "Example", username: "alice" },
        { entryId: "entry-b", siteName: "Example", username: "bob" },
      ],
      candidateId: candidate.candidateId,
      origin: candidate.origin,
    },
  });
  assert.deepEqual(harness.tabMessages.at(-1).message.payload.accounts.map((item) => item.entryId), ["entry-a", "entry-b"]);
  await assert.rejects(
    harness.sendContent({
      type: "petaldesk.password.save-decision",
      action: "replace",
      candidateId: candidate.candidateId,
      entryId: "entry-unknown",
    }, sender),
    /selected account/i,
  );
  const accepted = await harness.sendContent({
    type: "petaldesk.password.save-decision",
    action: "replace",
    candidateId: candidate.candidateId,
    entryId: "entry-b",
  }, sender);
  assert.equal(accepted.accepted, true);
  await harness.bridge.route({
    command: "password.saveResult",
    payload: {
      action: "replace",
      candidateId: candidate.candidateId,
      success: false,
      error: { code: "password_vault_locked", message: "保险库已锁定" },
    },
  });
  assert.equal((await harness.bridge.route({ command: "password.getStatus", payload: {} })).pendingCandidates, 1);
  assert.equal(harness.tabMessages.at(-1).message.command, "captureSaveResult");
  await harness.sendContent({
    type: "petaldesk.password.save-decision",
    action: "replace",
    candidateId: candidate.candidateId,
    entryId: "entry-b",
  }, sender);
  await harness.bridge.route({
    command: "password.saveResult",
    payload: { action: "replace", candidateId: candidate.candidateId, success: true, entryId: "entry-b" },
  });
  assert.equal((await harness.bridge.route({ command: "password.getStatus", payload: {} })).pendingCandidates, 0);
});

test("template recording opens an exact-origin tab and returns a constrained template", async () => {
  const harness = loadBridge();
  const opened = await harness.bridge.route({
    command: "password.startTemplateRecording",
    payload: {
      entryId: "entry-template",
      origin: "https://accounts.google.com",
      sessionId: "recording-1",
      url: "https://accounts.google.com/signin",
    },
  });
  assert.equal(opened.state, "opening");
  const sender = {
    documentId: "recording-document",
    frameId: 0,
    tab: { id: opened.tabId, url: "https://accounts.google.com/signin" },
    url: "https://accounts.google.com/signin",
  };
  await harness.sendContent(
    {
      type: "petaldesk.password.tab-ready",
      origin: "https://accounts.google.com",
    },
    sender,
  );
  assert.equal(harness.tabMessages.at(-1).message.command, "templateRecordStart");
  assert.equal(harness.events.at(-1).event, "templateRecordingReady");

  const username = await harness.sendContent(
    {
      type: "petaldesk.password.template-recording-progress",
      field: "username",
      origin: "https://accounts.google.com",
      selector: 'input[name="identifier"]',
      sessionId: "recording-1",
    },
    sender,
  );
  assert.equal(username.completed, false);
  const password = await harness.sendContent(
    {
      type: "petaldesk.password.template-recording-progress",
      field: "password",
      origin: "https://accounts.google.com",
      selector: 'input[name="Passwd"]',
      sessionId: "recording-1",
    },
    sender,
  );
  assert.equal(password.completed, true);
  assert.deepEqual(JSON.parse(JSON.stringify(password.template)), {
    id: "recorded-entry-template",
    label: "用户录制模板",
    mode: "password",
    origin: "https://accounts.google.com",
    passwordSelectors: ['input[name="Passwd"]'],
    usernameSelectors: ['input[name="identifier"]'],
    version: 1,
  });
  assert.equal(harness.events.at(-1).event, "templateRecordingResult");
  assert.equal(harness.events.at(-1).payload.status, "completed");
  const status = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(status.pendingTemplateRecordings, 0);
});

test("HTTP origins require an explicit allowlist for opening and capture", async () => {
  const harness = loadBridge();
  await assert.rejects(
    harness.bridge.route({
      command: "password.open",
      payload: { entryId: "entry", origin: "http://intranet.test", sessionId: "http-session", url: "http://intranet.test/login" },
    }),
    /HTTP.*opt-in/i,
  );
  const opened = await harness.bridge.route({
    command: "password.open",
    payload: {
      allowInsecureHttp: true,
      entryId: "entry",
      origin: "http://intranet.test",
      sessionId: "http-session",
      url: "http://intranet.test/login",
    },
  });
  assert.equal(opened.origin, "http://intranet.test");
});

test("a trusted iframe confirms the broadcast offer and receives the secret alone", async () => {
  const harness = loadBridge();
  harness.tabs.set(88, { id: 88, url: "https://mail.163.com/" });
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      documentId: "document-88",
      entryId: "entry-163",
      offerId: "offer-163",
      origin: "https://mail.163.com",
      sessionId: "iframe-fill",
      tabId: 88,
      username: "alice@163.com",
    },
  });
  const offer = harness.tabMessages.at(-1);
  assert.equal(offer.message.command, "fillOffer");
  // The offer is broadcast: no frameId, every frame decides for itself.
  assert.equal(offer.options, undefined);
  assert.equal(offer.message.payload.origin, "https://mail.163.com");
  const iframeSender = {
    documentId: "iframe-document-88",
    frameId: 7,
    tab: { id: 88, url: "https://mail.163.com/" },
    url: "https://dl.reg.163.com/login",
  };
  const confirmed = await harness.sendContent(
    {
      type: "petaldesk.password.fill-confirm",
      frameOrigin: "https://dl.reg.163.com",
      offerId: "offer-163",
      origin: "https://mail.163.com",
      sessionId: "iframe-fill",
    },
    iframeSender,
  );
  assert.equal(confirmed.confirmed, true);
  const fillConfirm = harness.events.at(-1);
  assert.equal(fillConfirm.event, "fillConfirm");
  assert.equal(fillConfirm.payload.frameId, 7);
  assert.equal(fillConfirm.payload.frameOrigin, "https://dl.reg.163.com");
  assert.equal(fillConfirm.payload.origin, "https://mail.163.com");
  const credentials = {
    offerId: "offer-163",
    origin: "https://mail.163.com",
    password: "secret-password",
    sessionId: "iframe-fill",
  };
  const filled = await harness.bridge.route({ command: "password.provideCredentials", payload: credentials });
  assert.equal(filled.filledPassword, true);
  assert.equal(credentials.password, "");
  const secret = harness.tabMessages.at(-1);
  assert.equal(secret.message.command, "fillSecret");
  // The password is delivered only to the confirmed trusted frame.
  assert.equal(secret.options.frameId, 7);
  const iframeResult = harness.events.at(-1);
  assert.equal(iframeResult.event, "fillResult");
  assert.equal(iframeResult.payload.frameId, 7);
  assert.equal(iframeResult.payload.frameOrigin, "https://dl.reg.163.com");
  assert.equal(iframeResult.payload.origin, "https://mail.163.com");
});

test("known login frames receive a direct fill offer before the broadcast fallback", async () => {
  const harness = loadBridge();
  harness.tabs.set(90, { id: 90, url: "https://mail.163.com/" });
  await harness.sendContent(
    {
      type: "petaldesk.password.tab-ready",
      hasPassword: false,
      hasUsername: false,
      origin: "https://mail.163.com",
    },
    {
      documentId: "document-90",
      frameId: 0,
      tab: { id: 90, url: "https://mail.163.com/" },
      url: "https://mail.163.com/",
    },
  );
  await harness.sendContent(
    {
      type: "petaldesk.password.frame-state",
      hasPassword: true,
      hasUsername: true,
      origin: "https://dl.reg.163.com",
    },
    {
      documentId: "iframe-document-90",
      frameId: 7,
      tab: { id: 90, url: "https://mail.163.com/" },
      url: "https://dl.reg.163.com/login",
    },
  );
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      documentId: "document-90",
      entryId: "entry-163",
      offerId: "offer-targeted-90",
      origin: "https://mail.163.com",
      sessionId: "targeted-90",
      tabId: 90,
      username: "alice@163.com",
    },
  });
  const offer = harness.tabMessages.at(-1);
  assert.equal(offer.message.command, "fillOffer");
  assert.equal(offer.options.frameId, 7);
});

test("a Firefox iframe confirmation may report its own origin without ancestorOrigins", async () => {
  const harness = loadBridge();
  harness.tabs.set(89, { id: 89, url: "https://mail.163.com/" });
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      documentId: "document-89",
      entryId: "entry-163",
      offerId: "offer-firefox-origin",
      origin: "https://mail.163.com",
      sessionId: "iframe-firefox-origin",
      tabId: 89,
      username: "alice@163.com",
    },
  });
  const confirmed = await harness.sendContent(
    {
      type: "petaldesk.password.fill-confirm",
      frameOrigin: "https://dl.reg.163.com",
      // Firefox 140-147 has no location.ancestorOrigins, so the content
      // script falls back to its own frame origin here.
      origin: "https://dl.reg.163.com",
      offerId: "offer-firefox-origin",
      sessionId: "iframe-firefox-origin",
    },
    {
      documentId: "iframe-document-89",
      frameId: 7,
      tab: { id: 89, url: "https://mail.163.com/" },
      url: "https://dl.reg.163.com/login",
    },
  );
  assert.equal(confirmed.confirmed, true);
  assert.equal(harness.events.at(-1).payload.origin, "https://mail.163.com");
  assert.equal(harness.events.at(-1).payload.frameOrigin, "https://dl.reg.163.com");
});

test("top-frame readiness does not erase an earlier iframe capability snapshot", async () => {
  const harness = loadBridge();
  harness.tabs.set(90, { id: 90, url: "https://mail.163.com/" });
  const iframeSender = {
    documentId: "iframe-document-90",
    frameId: 7,
    tab: { id: 90, url: "https://mail.163.com/" },
    url: "https://dl.reg.163.com/login",
  };
  await harness.sendContent(
    {
      type: "petaldesk.password.frame-state",
      origin: "https://dl.reg.163.com",
      hasPassword: true,
      hasUsername: true,
    },
    iframeSender,
  );
  await harness.sendContent(
    {
      type: "petaldesk.password.tab-ready",
      origin: "https://mail.163.com",
    },
    {
      documentId: "top-document-90",
      frameId: 0,
      tab: { id: 90, url: "https://mail.163.com/" },
      url: "https://mail.163.com/",
    },
  );
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      entryId: "entry-163",
      offerId: "offer-ready-order",
      origin: "https://mail.163.com",
      sessionId: "ready-order",
      tabId: 90,
      username: "alice@163.com",
    },
  });
  const offer = harness.tabMessages.at(-1);
  assert.equal(offer.message.command, "fillOffer");
  assert.equal(offer.options.frameId, 7);
});

test("a trusted iframe stays bound across a two-step fill", async () => {
  const harness = loadBridge();
  harness.tabs.set(76, { id: 76, url: "https://mail.163.com/" });
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      entryId: "entry-163",
      offerId: "offer-step-1",
      origin: "https://mail.163.com",
      sessionId: "iframe-two-step",
      tabId: 76,
      username: "alice@163.com",
    },
  });
  const iframeSender = {
    documentId: "iframe-document-76",
    frameId: 7,
    tab: { id: 76, url: "https://mail.163.com/" },
    url: "https://dl.reg.163.com/login",
  };
  await harness.sendContent(
    {
      type: "petaldesk.password.fill-confirm",
      frameOrigin: "https://dl.reg.163.com",
      offerId: "offer-step-1",
      origin: "https://mail.163.com",
      sessionId: "iframe-two-step",
    },
    iframeSender,
  );
  const originalSendTabMessage = harness.api.sendTabMessage;
  let firstSecret = true;
  harness.api.sendTabMessage = async (tabId, message, options) => {
    const response = await originalSendTabMessage(tabId, message, options);
    if (message.command === "fillSecret" && firstSecret) {
      firstSecret = false;
      return { ok: true, result: { ...response.result, filledPassword: false, needsNextStep: true } };
    }
    return response;
  };
  const first = await harness.bridge.route({
    command: "password.provideCredentials",
    payload: {
      offerId: "offer-step-1",
      origin: "https://mail.163.com",
      password: "secret-password",
      sessionId: "iframe-two-step",
    },
  });
  assert.equal(first.needsNextStep, true);
  // The second offer must still reach the iframe-bound session and confirm.
  await harness.bridge.route({
    command: "password.offerFill",
    payload: {
      offerId: "offer-step-2",
      origin: "https://mail.163.com",
      sessionId: "iframe-two-step",
      username: "alice@163.com",
    },
  });
  const reconfirmed = await harness.sendContent(
    {
      type: "petaldesk.password.fill-confirm",
      frameOrigin: "https://dl.reg.163.com",
      offerId: "offer-step-2",
      origin: "https://mail.163.com",
      sessionId: "iframe-two-step",
    },
    iframeSender,
  );
  assert.equal(reconfirmed.confirmed, true);
  await harness.bridge.route({
    command: "password.provideCredentials",
    payload: {
      offerId: "offer-step-2",
      origin: "https://mail.163.com",
      password: "secret-password",
      sessionId: "iframe-two-step",
    },
  });
  const secret = harness.tabMessages.at(-1);
  assert.equal(secret.message.command, "fillSecret");
  assert.equal(secret.options.frameId, 7);
});

test("a cross-site iframe confirmation is rejected and clears the fill session", async () => {
  const harness = loadBridge();
  harness.tabs.set(89, { id: 89, url: "https://mail.163.com/" });
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      entryId: "entry-163",
      offerId: "offer-xsite",
      origin: "https://mail.163.com",
      sessionId: "iframe-xsite",
      tabId: 89,
      username: "alice@163.com",
    },
  });
  const evilSender = {
    documentId: "evil-document",
    frameId: 9,
    tab: { id: 89, url: "https://mail.163.com/" },
    url: "https://evil.example/phish",
  };
  await assert.rejects(
    harness.sendContent(
      {
        type: "petaldesk.password.fill-confirm",
        frameOrigin: "https://evil.example",
        offerId: "offer-xsite",
        origin: "https://mail.163.com",
        sessionId: "iframe-xsite",
      },
      evilSender,
    ),
    /trusted/i,
  );
  assert.equal(harness.events.some((event) => event.event === "fillConfirm"), false);
  // The session was dropped: no credentials can be provided afterwards.
  await assert.rejects(
    harness.bridge.route({
      command: "password.provideCredentials",
      payload: {
        offerId: "offer-xsite",
        origin: "https://mail.163.com",
        password: "secret-password",
        sessionId: "iframe-xsite",
      },
    }),
    /expired/i,
  );
  assert.equal(harness.tabMessages.some((entry) => entry.message.command === "fillSecret"), false);
});

test("a shared-hosting tenant iframe cannot confirm another tenant's fill", async () => {
  const harness = loadBridge();
  harness.tabs.set(96, { id: 96, url: "https://victim.github.io/login" });
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      entryId: "entry-tenant",
      offerId: "offer-tenant",
      origin: "https://victim.github.io",
      sessionId: "iframe-tenant",
      tabId: 96,
      username: "alice",
    },
  });
  await assert.rejects(
    harness.sendContent(
      {
        type: "petaldesk.password.fill-confirm",
        frameOrigin: "https://attacker.github.io",
        offerId: "offer-tenant",
        origin: "https://victim.github.io",
        sessionId: "iframe-tenant",
      },
      {
        documentId: "attacker-document",
        frameId: 12,
        tab: { id: 96, url: "https://victim.github.io/login" },
        url: "https://attacker.github.io/phish",
      },
    ),
    /trusted/i,
  );
  assert.equal(harness.events.some((event) => event.event === "fillConfirm"), false);
});

test("a trusted iframe cannot confirm after the top-level origin changes", async () => {
  const harness = loadBridge();
  harness.tabs.set(97, { id: 97, url: "https://mail.163.com/" });
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      entryId: "entry-navigation",
      offerId: "offer-navigation",
      origin: "https://mail.163.com",
      sessionId: "iframe-navigation",
      tabId: 97,
      username: "alice@163.com",
    },
  });
  harness.tabs.set(97, { id: 97, url: "https://evil.example/" });
  await assert.rejects(
    harness.sendContent(
      {
        type: "petaldesk.password.fill-confirm",
        frameOrigin: "https://dl.reg.163.com",
        offerId: "offer-navigation",
        origin: "https://dl.reg.163.com",
        sessionId: "iframe-navigation",
      },
      {
        documentId: "stale-iframe-document",
        frameId: 7,
        tab: { id: 97, url: "https://evil.example/" },
        url: "https://dl.reg.163.com/login",
      },
    ),
    /trusted|does not match/i,
  );
  assert.equal(harness.events.some((event) => event.event === "fillConfirm"), false);
});

test("a forged frameOrigin that disagrees with the sender URL is rejected", async () => {
  const harness = loadBridge();
  harness.tabs.set(93, { id: 93, url: "https://mail.163.com/" });
  await harness.bridge.route({
    command: "password.offerFillDirect",
    payload: {
      entryId: "entry-163",
      offerId: "offer-forged",
      origin: "https://mail.163.com",
      sessionId: "iframe-forged",
      tabId: 93,
      username: "alice@163.com",
    },
  });
  const forgedSender = {
    documentId: "forged-frame-document",
    frameId: 11,
    tab: { id: 93, url: "https://mail.163.com/" },
    url: "https://evil.example/phish",
  };
  await assert.rejects(
    harness.sendContent(
      {
        type: "petaldesk.password.fill-confirm",
        frameOrigin: "https://dl.reg.163.com",
        offerId: "offer-forged",
        origin: "https://mail.163.com",
        sessionId: "iframe-forged",
      },
      forgedSender,
    ),
    /does not match/i,
  );
  assert.equal(harness.events.some((event) => event.event === "fillConfirm"), false);
});

test("a trusted iframe capture candidate promotes with its frame origin", async () => {
  const harness = loadBridge();
  harness.tabs.set(92, { id: 92, url: "https://mail.163.com/" });
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const iframeSender = {
    documentId: "iframe-document-92",
    frameId: 7,
    tab: { id: 92, url: "https://mail.163.com/" },
    url: "https://dl.reg.163.com/login",
  };
  const candidate = {
    candidateId: "iframe-candidate",
    frameOrigin: "https://dl.reg.163.com",
    // Firefox 140-147 lacks ancestorOrigins, so the content script reports
    // its own frame origin and the background derives the top-level origin.
    origin: "https://dl.reg.163.com",
    password: "iframe-secret",
    username: "alice",
  };
  const accepted = await harness.sendContent(
    { type: "petaldesk.password.capture-submitted", candidate },
    iframeSender,
  );
  assert.equal(accepted.accepted, true);
  assert.equal(candidate.password, "");
  const promoted = await harness.sendContent(
    {
      type: "petaldesk.password.capture-success",
      candidateId: "iframe-candidate",
      confidence: "high",
      origin: "https://mail.163.com",
    },
    iframeSender,
  );
  assert.equal(promoted.promoted, true);
  const nativeCandidate = harness.events.at(-1);
  assert.equal(nativeCandidate.event, "captureCandidate");
  assert.equal(nativeCandidate.payload.origin, "https://mail.163.com");
  assert.equal(nativeCandidate.payload.frameOrigin, "https://dl.reg.163.com");
  assert.equal(nativeCandidate.payload.promptOrigin, "https://mail.163.com");
  assert.equal(nativeCandidate.payload.frameId, 7);
  assert.equal(nativeCandidate.payload.password, "iframe-secret");

  await harness.bridge.route({
    command: "password.captureMatch",
    payload: { action: "new", candidateId: "iframe-candidate" },
  });
  const decision = await harness.sendContent(
    {
      type: "petaldesk.password.save-decision",
      action: "new",
      candidateId: "iframe-candidate",
    },
    iframeSender,
  );
  assert.equal(decision.accepted, true);
  const saveEvent = harness.events.at(-1);
  assert.equal(saveEvent.event, "saveDecision");
  assert.equal(saveEvent.payload.origin, "https://mail.163.com");
  assert.equal(saveEvent.payload.promptOrigin, "https://mail.163.com");
});

test("a cross-site iframe capture candidate is discarded", async () => {
  const harness = loadBridge();
  harness.tabs.set(94, { id: 94, url: "https://mail.163.com/" });
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const evilSender = {
    documentId: "evil-capture-document",
    frameId: 9,
    tab: { id: 94, url: "https://mail.163.com/" },
    url: "https://evil.example/login",
  };
  await assert.rejects(
    harness.sendContent(
      {
        type: "petaldesk.password.capture-submitted",
        candidate: {
          candidateId: "xsite-candidate",
          frameOrigin: "https://evil.example",
          origin: "https://mail.163.com",
          password: "stolen",
          username: "alice",
        },
      },
      evilSender,
    ),
    /cross-site|origin/i,
  );
  const status = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(status.pendingCandidates, 0);
  assert.equal(harness.events.some((event) => event.event === "captureCandidate"), false);
});

test("a same-origin refresh replays the cached badge without waiting for the desktop", async () => {
  const harness = loadBridge();
  harness.tabs.set(73, { id: 73, url: "https://example.test/login" });
  await harness.bridge.route({
    command: "password.updateBadge",
    payload: {
      accounts: [
        { entryId: "entry-a", siteName: "Example", username: "alice" },
        { entryId: "entry-b", siteName: "Example", username: "bob" },
      ],
      origin: "https://example.test",
      tabId: 73,
    },
  });
  assert.deepEqual(harness.actionUpdates.badgeTexts.at(-1), { tabId: 73, text: "2" });
  const sender = {
    documentId: "document-73-refresh",
    frameId: 0,
    tab: { id: 73, url: "https://example.test/login" },
    url: "https://example.test/login",
  };
  await harness.sendContent(
    { type: "petaldesk.password.tab-ready", origin: "https://example.test" },
    sender,
  );
  assert.equal(harness.events.at(-1).event, "originActive");
  // The cached count is re-applied immediately after the same-origin refresh.
  assert.deepEqual(harness.actionUpdates.badgeTexts.at(-1), { tabId: 73, text: "2" });
});

test("capture enable broadcasts to every frame of the tab", async () => {
  const harness = loadBridge();
  harness.tabs.set(74, { id: 74, url: "https://example.test/login" });
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  const enable = harness.tabMessages.find((entry) => entry.message.command === "captureEnable");
  assert.ok(enable, "the capture enable command should be sent");
  assert.equal(enable.options, undefined);
  assert.equal(enable.message.payload.topLevelOrigin, "https://example.test");
});

test("an iframe tab-ready learns the capture state without tab bookkeeping", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  harness.tabs.set(75, { id: 75, url: "https://mail.163.com/" });
  const iframeSender = {
    documentId: "iframe-document-75",
    frameId: 4,
    tab: { id: 75, url: "https://mail.163.com/" },
    url: "https://dl.reg.163.com/login",
  };
  const ready = await harness.sendContent(
    { type: "petaldesk.password.tab-ready", origin: "https://dl.reg.163.com" },
    iframeSender,
  );
  assert.equal(ready.captureEnabled, true);
  assert.equal(ready.topLevelOrigin, "https://mail.163.com");
  // No originActive event and no badge/bookkeeping for a non-top frame.
  assert.equal(harness.events.some((event) => event.event === "originActive"), false);
  await assert.rejects(
    harness.sendContent(
      { type: "petaldesk.password.tab-ready", origin: "https://mail.163.com" },
      iframeSender,
    ),
    /did not match/i,
  );
  const topSender = {
    documentId: "document-75",
    frameId: 0,
    tab: { id: 75, url: "https://mail.163.com/" },
    url: "https://mail.163.com/",
  };
  await harness.sendContent(
    { type: "petaldesk.password.tab-ready", origin: "https://mail.163.com" },
    topSender,
  );
  assert.equal(harness.events.at(-1).event, "originActive");
  assert.deepEqual(harness.events.at(-1).payload, { origin: "https://mail.163.com", tabId: 75 });
});

test("an undeclared cross-origin iframe never learns an enabled capture state", async () => {
  const harness = loadBridge();
  await harness.bridge.route({ command: "password.setCaptureEnabled", payload: { enabled: true } });
  harness.tabs.set(98, { id: 98, url: "https://victim.github.io/login" });
  const ready = await harness.sendContent(
    { type: "petaldesk.password.tab-ready", origin: "https://attacker.github.io" },
    {
      documentId: "attacker-document-98",
      frameId: 8,
      tab: { id: 98, url: "https://victim.github.io/login" },
      url: "https://attacker.github.io/phish",
    },
  );
  assert.equal(ready.captureEnabled, false);
  assert.equal(ready.topLevelOrigin, "https://victim.github.io");
});
