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
    extensionVersion: "0.7.2",
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
      assert.equal(options.frameId, 0);
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
  assert.equal(harness.events.at(-1).event, "fillResult");
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
  assert.equal(state.diagnostics.extensionVersion, "0.7.2");
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
