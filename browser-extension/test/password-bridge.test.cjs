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

function loadBridge({ authenticationInfo = true } = {}) {
  const events = [];
  const runtimeMessages = eventTarget();
  const actionClicks = eventTarget();
  const permissionRemovals = eventTarget();
  const tabRemovals = eventTarget();
  const tabs = new Map();
  const tabMessages = [];
  let nextTabId = 40;
  let permissionRequests = 0;
  const api = {
    browserFamily: "firefox",
    action: { onClicked: actionClicks },
    permissions: { onRemoved: permissionRemovals },
    async createTab({ url }) {
      const tab = { id: ++nextTabId, url };
      tabs.set(tab.id, tab);
      return tab;
    },
    async getAllPermissions() {
      return authenticationInfo ? { data_collection: ["authenticationInfo"] } : { data_collection: [] };
    },
    async getTab(tabId) {
      return tabs.get(tabId) || { id: tabId, url: "https://accounts.google.com/signin" };
    },
    onTabRemoved(listener) {
      tabRemovals.addListener(listener);
      return true;
    },
    async queryAllTabs() {
      return Array.from(tabs.values());
    },
    async requestPermissions(value) {
      assert.deepEqual(JSON.parse(JSON.stringify(value)), { data_collection: ["authenticationInfo"] });
      permissionRequests += 1;
      authenticationInfo = true;
      return true;
    },
    runtime: { onMessage: runtimeMessages },
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
  return {
    actionClicks,
    api,
    bridge,
    events,
    permissionRequests: () => permissionRequests,
    revokeAuthenticationInfo() {
      authenticationInfo = false;
      for (const listener of permissionRemovals.listeners) {
        listener({ data_collection: ["authenticationInfo"] });
      }
    },
    sendContent,
    tabRemovals,
    tabMessages,
    tabs,
  };
}

test("consent request is armed by native command and requested only by toolbar gesture", async () => {
  const harness = loadBridge({ authenticationInfo: false });
  assert.deepEqual(JSON.parse(JSON.stringify(await harness.bridge.route({ command: "password.requestConsent", payload: {} }))), {
    actionRequired: "toolbar-click",
    granted: false,
    userGestureRequired: true,
  });
  assert.equal(harness.permissionRequests(), 0);
  harness.actionClicks.listeners[0]();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(harness.permissionRequests(), 1);
  assert.equal(harness.events.at(-1).event, "consentChanged");
  assert.equal(harness.events.at(-1).payload.granted, true);
  assert.equal(harness.events.at(-1).payload.actionRequired, null);
});

test("a direct fill attempt arms Firefox consent and succeeds only after the toolbar gesture", async () => {
  const harness = loadBridge({ authenticationInfo: false });
  const opened = await harness.bridge.route({
    command: "password.open",
    payload: {
      entryId: "entry-consent",
      origin: "https://accounts.google.com",
      sessionId: "session-consent",
      url: "https://accounts.google.com/signin",
    },
  });
  assert.equal(opened.authenticationConsent, false);
  assert.equal(opened.actionRequired, "toolbar-click");
  const status = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(status.consentArmed, true);
  assert.equal(status.consentActionRequired, "toolbar-click");
  const sender = {
    documentId: "consent-document",
    frameId: 0,
    tab: { id: opened.tabId, url: "https://accounts.google.com/signin" },
    url: "https://accounts.google.com/signin",
  };
  await harness.sendContent(
    { type: "petaldesk.password.tab-ready", origin: "https://accounts.google.com" },
    sender,
  );
  await assert.rejects(
    harness.bridge.route({
      command: "password.offerFill",
      payload: {
        entryId: "entry-consent",
        offerId: "offer-consent",
        origin: "https://accounts.google.com",
        sessionId: "session-consent",
        username: "alice@example.com",
      },
    }),
    /toolbar button/i,
  );
  harness.actionClicks.listeners[0]();
  await new Promise((resolve) => setImmediate(resolve));
  const offered = await harness.bridge.route({
    command: "password.offerFill",
    payload: {
      entryId: "entry-consent",
      offerId: "offer-consent",
      origin: "https://accounts.google.com",
      sessionId: "session-consent",
      username: "alice@example.com",
    },
  });
  assert.equal(offered.state, "fillOffer");
});

test("revoking Firefox authentication permission disables capture and clears sessions", async () => {
  const harness = loadBridge();
  const opened = await harness.bridge.route({
    command: "password.open",
    payload: {
      entryId: "entry-revoked",
      origin: "https://accounts.google.com",
      sessionId: "session-revoked",
      url: "https://accounts.google.com/signin",
    },
  });
  await harness.bridge.route({
    command: "password.setCaptureEnabled",
    payload: { enabled: true },
  });
  const before = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(before.authenticationConsent, true);
  assert.equal(before.captureEnabled, true);
  assert.equal(before.pendingFillSessions, 1);

  harness.revokeAuthenticationInfo();
  await new Promise((resolve) => setImmediate(resolve));
  const after = await harness.bridge.route({ command: "password.getStatus", payload: {} });
  assert.equal(after.authenticationConsent, false);
  assert.equal(after.captureEnabled, false);
  assert.equal(after.pendingFillSessions, 0);
  assert.equal(harness.events.at(-1).event, "consentChanged");
  assert.equal(harness.events.at(-1).payload.granted, false);
  assert.ok(harness.tabMessages.some(({ tabId, message }) => (
    tabId === opened.tabId && message.command === "captureDisable"
  )));
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
