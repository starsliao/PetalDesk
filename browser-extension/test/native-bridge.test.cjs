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

function loadBridge(family = "chrome") {
  const postedMessages = [];
  const nativeMessages = eventTarget();
  const nativeDisconnect = eventTarget();
  const runtimeStartup = eventTarget();
  const runtimeInstalled = eventTarget();
  const tabMessages = [];
  let selectedTabId = 7;
  const port = {
    onMessage: nativeMessages,
    onDisconnect: nativeDisconnect,
    postMessage(message) {
      postedMessages.push(message);
    },
  };
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
      return port;
    },
    getManifest() {
      return { version: "0.8.0" };
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
    console,
    navigator: {
      userAgent:
        family === "firefox"
          ? "Firefox/154.0"
          : family === "edge"
            ? "Chrome/140.0 Edg/140.0"
            : "Chrome/140.0",
    },
    setTimeout,
  };
  let passwordDisconnects = 0;
  let secretConnects = 0;
  const nativeStates = [];
  if (family === "firefox") {
    context.PetalDeskPasswordBridge = {
      createPasswordBridge() {
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
    nativeDisconnect,
    nativeMessages,
    nativeStates: () => Array.from(nativeStates),
    postedMessages,
    tabMessages,
    selectTab(tabId) {
      selectedTabId = tabId;
    },
    passwordDisconnects: () => passwordDisconnects,
    secretConnects: () => secretConnects,
  };
}

async function sendNativeRequest(bridge, request) {
  bridge.nativeMessages.listeners[0](request);
  await new Promise((resolve) => setImmediate(resolve));
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
  assert.equal(bridge.postedMessages.length, 1);
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
