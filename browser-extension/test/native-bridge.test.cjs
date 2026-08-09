const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const protocolFixture = JSON.parse(
  fs.readFileSync(path.join(__dirname, "fixtures", "native-protocol.json"), "utf8"),
);

function eventTarget() {
  const listeners = [];
  return {
    listeners,
    addListener(listener) {
      listeners.push(listener);
    },
  };
}

function loadBridge(family = "chrome", { timerCapMs = null } = {}) {
  const postedMessages = [];
  const ports = [];
  const runtimeStartup = eventTarget();
  const runtimeInstalled = eventTarget();
  const tabMessages = [];
  let selectedTabId = 7;
  let remainingPostFailures = 0;
  function createPort() {
    const nativeMessages = eventTarget();
    const nativeDisconnect = eventTarget();
    const port = {
      nativeDisconnect,
      nativeMessages,
      onMessage: nativeMessages,
      onDisconnect: nativeDisconnect,
      disconnect() {
        for (const listener of nativeDisconnect.listeners) listener();
      },
      postMessage(message) {
        if (remainingPostFailures > 0) {
          remainingPostFailures -= 1;
          throw new Error("simulated native post failure");
        }
        postedMessages.push(message);
      },
    };
    ports.push(port);
    return port;
  }
  const runtime = {
    id:
      family === "firefox"
        ? "petaldesk-capture@petaldesk.app"
        : "abcdefghijklmnopabcdefghijklmnop",
    lastError: null,
    onInstalled: runtimeInstalled,
    onStartup: runtimeStartup,
    connectNative(hostName) {
      assert.equal(hostName, "com.petaldesk.capture");
      return createPort();
    },
    getManifest() {
      return { version: "0.8.3" };
    },
  };
  const callbackTabs = {
    query(_query, done) {
      done([{ id: selectedTabId }]);
    },
    sendMessage(tabId, message, options, done) {
      tabMessages.push({ tabId, message, options });
      done({ ok: true, result: { state: "idle" } });
    },
  };
  const promiseTabs = {
    async query() {
      return [{ id: selectedTabId }];
    },
    async sendMessage(tabId, message, options) {
      tabMessages.push({ tabId, message, options });
      return { ok: true, result: { state: "idle" } };
    },
  };
  const context = {
    clearTimeout,
    console: { warn() {} },
    navigator: {
      userAgent:
        family === "firefox"
          ? "Firefox/154.0"
          : family === "edge"
            ? "Chrome/140.0 Edg/140.0"
            : "Chrome/140.0",
    },
    setTimeout(callback, delay, ...args) {
      const effectiveDelay = timerCapMs == null || delay >= 30_000
        ? delay
        : Math.min(delay, timerCapMs);
      return setTimeout(callback, effectiveDelay, ...args);
    },
  };
  let passwordDisconnects = 0;
  let secretConnects = 0;
  let manualReconnect = null;
  const nativeStates = [];
  const connectionStates = [];
  if (family === "firefox") {
    context.PetalDeskPasswordBridge = {
      createPasswordBridge(options) {
        manualReconnect = options.requestNativeReconnect;
        return {
          capabilities: [],
          disconnect() {
            passwordDisconnects += 1;
          },
          onSecretConnected() {
            secretConnects += 1;
            return Promise.resolve();
          },
          route() {
            throw new Error("not used by this harness");
          },
          setNativeConnected(connected) {
            nativeStates.push(connected === true);
          },
          setNativeConnectionState(state) {
            const snapshot = JSON.parse(JSON.stringify(state));
            connectionStates.push(snapshot);
            nativeStates.push(snapshot.connected === true);
          },
          supportsCommand() {
            return false;
          },
        };
      },
    };
  }
  if (family === "firefox") {
    context.browser = { runtime, tabs: promiseTabs };
  } else {
    context.chrome = { runtime, tabs: callbackTabs };
  }
  context.globalThis = context;
  vm.createContext(context);

  for (const source of [
    "src/shared/browser-api.js",
    "src/background/native-bridge.js",
  ]) {
    const code = fs.readFileSync(path.join(__dirname, "..", source), "utf8");
    vm.runInContext(code, context, { filename: source });
  }

  return {
    connectionStates: () => Array.from(connectionStates, (state) => ({ ...state })),
    failNextPosts(count) {
      remainingPostFailures = count;
    },
    manualReconnect() {
      assert.equal(typeof manualReconnect, "function");
      return manualReconnect();
    },
    nativeDisconnect: ports[0].nativeDisconnect,
    nativeMessages: ports[0].nativeMessages,
    nativeStates: () => Array.from(nativeStates),
    ports,
    postedMessages,
    tabMessages,
    selectTab(tabId) {
      selectedTabId = tabId;
    },
    passwordDisconnects: () => passwordDisconnects,
    secretConnects: () => secretConnects,
  };
}

async function sendNativeRequest(bridge, request, port = bridge.ports.at(-1)) {
  port.nativeMessages.listeners[0](request);
  await new Promise((resolve) => setImmediate(resolve));
}

async function waitForCondition(condition, message, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (condition()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail(message);
}

test("native protocol handshakes and routes a command end to end", async () => {
  const bridge = loadBridge();
  assert.deepEqual(
    JSON.parse(JSON.stringify(bridge.postedMessages[0])),
    protocolFixture.ready,
  );

  bridge.nativeMessages.listeners[0](protocolFixture.command);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(bridge.tabMessages.length, 1);
  assert.equal(bridge.tabMessages[0].message.command, "status");
  assert.deepEqual(
    JSON.parse(JSON.stringify(bridge.postedMessages[1])),
    protocolFixture.response,
  );
});

test("native protocol rejects an incompatible command version", async () => {
  const bridge = loadBridge();
  bridge.nativeMessages.listeners[0]({
    protocolVersion: 2,
    type: "command",
    id: "request-2",
    command: "status",
    payload: {},
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(bridge.tabMessages.length, 0);
  assert.equal(bridge.postedMessages[1].type, "extension.response");
  assert.equal(bridge.postedMessages[1].id, "request-2");
  assert.equal(bridge.postedMessages[1].ok, false);
  assert.match(bridge.postedMessages[1].error.message, /version/i);
});

test("secret pipe lifecycle disconnect clears the Firefox password bridge", async () => {
  const bridge = loadBridge("firefox");
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    type: "extension.event",
    event: "secretDisconnected",
    payload: { reason: "secret-pipe-unavailable" },
  });
  assert.equal(bridge.passwordDisconnects(), 1);
  const reconnecting = bridge.connectionStates().at(-1);
  assert.equal(reconnecting.state, "reconnecting");
  assert.equal(reconnecting.reconnectAttempt, 1);
  assert.equal(reconnecting.canReconnect, false);
});

test("secret pipe connect replays the active origin and tracks stdio state", async () => {
  const bridge = loadBridge("firefox");
  assert.deepEqual(bridge.nativeStates(), [true]);
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    type: "extension.event",
    event: "secretConnected",
    payload: {},
  });
  assert.equal(bridge.secretConnects(), 1);
  assert.equal(bridge.passwordDisconnects(), 0);
  bridge.nativeDisconnect.listeners[0]();
  assert.equal(bridge.nativeStates().at(-1), false);
  assert.equal(bridge.passwordDisconnects(), 1);
});

test("a secret-channel loss rebuilds the full native port and resets after secretConnected", async () => {
  const bridge = loadBridge("firefox");
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    type: "extension.event",
    event: "secretConnected",
    payload: {},
  });
  const initialPort = bridge.ports[0];

  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    type: "extension.event",
    event: "secretDisconnected",
    payload: { reason: "pipe-closed" },
  }, initialPort);
  await new Promise((resolve) => setTimeout(resolve, 10));

  assert.equal(bridge.ports.length, 2);
  const reconnecting = bridge.connectionStates().at(-1);
  assert.equal(reconnecting.state, "reconnecting");
  assert.equal(reconnecting.reconnectAttempt, 1);
  assert.equal(reconnecting.canReconnect, false);

  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    type: "extension.event",
    event: "secretConnected",
    payload: {},
  });
  const connected = bridge.connectionStates().at(-1);
  assert.equal(connected.state, "connected");
  assert.equal(connected.reconnectAttempt, 0);
  assert.equal(connected.canReconnect, false);
});

test("three quick failures expose manual reconnect only after the third attempt", async () => {
  const bridge = loadBridge("firefox", { timerCapMs: 10 });
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    type: "extension.event",
    event: "secretConnected",
    payload: {},
  });

  bridge.ports[0].nativeDisconnect.listeners[0]();
  await waitForCondition(
    () => {
      const state = bridge.connectionStates().at(-1);
      return state?.state === "disconnected"
        && state.reconnectAttempt === 3
        && state.canReconnect === true
        && bridge.ports.length === 4;
    },
    "fast reconnect cycle should expose manual reconnect after three failures",
  );

  const fastStates = bridge.connectionStates().filter((state) => state.state === "reconnecting");
  for (const attempt of [1, 2, 3]) {
    const state = fastStates.find((candidate) => candidate.reconnectAttempt === attempt);
    assert.ok(state, `attempt ${attempt} should be observable`);
    assert.equal(state.canReconnect, false);
  }
  const exhausted = bridge.connectionStates().at(-1);
  assert.equal(exhausted.state, "disconnected");
  assert.equal(exhausted.reconnectAttempt, 3);
  assert.equal(exhausted.canReconnect, true);
  assert.equal(bridge.ports.length, 4);

  const manual = bridge.manualReconnect();
  assert.equal(manual.state, "reconnecting");
  assert.equal(manual.reconnectAttempt, 1);
  assert.equal(manual.canReconnect, false);
  await new Promise((resolve) => setTimeout(resolve, 1));
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    type: "extension.event",
    event: "secretConnected",
    payload: {},
  });
  const recovered = bridge.connectionStates().at(-1);
  assert.equal(recovered.state, "connected");
  assert.equal(recovered.reconnectAttempt, 0);
  assert.equal(recovered.canReconnect, false);
});

test("native post failures consume one reconnect attempt each without a reconnect storm", async () => {
  const bridge = loadBridge("firefox", { timerCapMs: 5 });
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    type: "extension.event",
    event: "secretConnected",
    payload: {},
  });
  bridge.failNextPosts(3);
  bridge.ports[0].nativeDisconnect.listeners[0]();
  await new Promise((resolve) => setTimeout(resolve, 80));

  const attempts = new Set(
    bridge.connectionStates()
      .filter((state) => state.state === "reconnecting")
      .map((state) => state.reconnectAttempt),
  );
  assert.deepEqual(Array.from(attempts).filter(Boolean).sort(), [1, 2, 3]);
  assert.equal(bridge.ports.length, 4);
  const exhausted = bridge.connectionStates().at(-1);
  assert.equal(exhausted.state, "disconnected");
  assert.equal(exhausted.canReconnect, true);
});

test("Chrome and Edge mark replacement native ports healthy without Firefox secret events", async () => {
  for (const family of ["chrome", "edge"]) {
    const bridge = loadBridge(family, { timerCapMs: 2 });
    assert.equal(bridge.connectionStates().length, 0);
    bridge.ports[0].nativeDisconnect.listeners[0]();
    await new Promise((resolve) => setTimeout(resolve, 5));
    assert.equal(bridge.ports.length, 2);
    assert.equal(bridge.postedMessages.filter((message) => message.type === "extension.ready").length, 2);
  }
});

test("capture commands stay bound to the tab and frame selected by prepare", async () => {
  const bridge = loadBridge();
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    id: "prepare-fixed-target",
    command: "prepare",
    payload: { tabId: 42, frameId: 3, anchor: { x: 10, y: 20 } },
  });

  bridge.selectTab(99);
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    id: "start-fixed-target",
    command: "start",
    payload: {},
  });
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    id: "status-fixed-target",
    command: "status",
    payload: {},
  });
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    id: "restore-fixed-target",
    command: "restore",
    payload: {},
  });
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    id: "status-after-restore",
    command: "status",
    payload: {},
  });

  assert.deepEqual(
    bridge.tabMessages.map(({ tabId, options }) => [tabId, options.frameId]),
    [
      [42, 3],
      [42, 3],
      [42, 3],
      [42, 3],
      [99, 0],
    ],
  );
  assert.equal(bridge.postedMessages.at(-1).result.tabId, 99);
  assert.equal(bridge.postedMessages.at(-1).result.frameId, 0);
});

test("top-level routing IDs override payload routing IDs without changing tabs", async () => {
  const bridge = loadBridge("firefox");
  await sendNativeRequest(bridge, {
    protocolVersion: 1,
    id: "explicit-target",
    command: "prepare",
    tabId: 17,
    frameId: 2,
    payload: { tabId: 88, frameId: 9 },
  });

  assert.equal(bridge.tabMessages[0].tabId, 17);
  assert.equal(bridge.tabMessages[0].options.frameId, 2);
});

test("browser adapter covers Edge and Firefox Native Messaging routes", async () => {
  for (const family of ["edge", "firefox"]) {
    const bridge = loadBridge(family);
    assert.equal(bridge.postedMessages[0].type, "extension.ready");
    assert.equal(bridge.postedMessages[0].browser, family);
    bridge.nativeMessages.listeners[0](protocolFixture.command);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(bridge.tabMessages.length, 1);
    assert.equal(bridge.postedMessages[1].type, "extension.response");
    assert.equal(bridge.postedMessages[1].ok, true);
  }
});
