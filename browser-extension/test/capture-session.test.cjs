const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

function createStyle() {
  const properties = new Map();
  const writes = [];
  return {
    writes,
    getPropertyPriority(name) {
      return properties.get(name)?.priority || "";
    },
    getPropertyValue(name) {
      return properties.get(name)?.value || "";
    },
    removeProperty(name) {
      properties.delete(name);
      writes.push({ action: "remove", name });
    },
    setProperty(name, value, priority = "") {
      properties.set(name, { value: String(value), priority: String(priority) });
      writes.push({ action: "set", name, value: String(value), priority: String(priority) });
    },
  };
}

function loadCaptureSession({ devicePixelRatio = 1.5, originalScrollTop = 120 } = {}) {
  const watchdogTimers = new Map();
  const createdStyles = [];
  const messageListeners = [];
  let timerSequence = 0;
  let visibleElements = [];

  function currentVisibleElements() {
    return typeof visibleElements === "function" ? visibleElements() : visibleElements;
  }

  class FakeElement {
    constructor({
      clientHeight = 0,
      height = 20,
      overflowY = "visible",
      position = "static",
      scrollHeight = 0,
      top = 0,
      width = 100,
    } = {}) {
      this.clientHeight = clientHeight;
      this.dataset = {};
      this.isConnected = true;
      this.overflowY = overflowY;
      this.parentElement = null;
      this.position = position;
      this.scrollHeight = scrollHeight;
      this.scrollTop = 0;
      this.style = createStyle();
      this.textContent = "";
      this.rect = { bottom: top + height, height, left: 0, right: width, top, width };
    }

    getBoundingClientRect() {
      return this.rect;
    }

    getRootNode() {
      return document;
    }

    remove() {
      this.isConnected = false;
    }
  }

  class FakeImageElement extends FakeElement {
    constructor(options) {
      super(options);
      this.complete = true;
    }
  }

  const documentElement = new FakeElement({ width: 800, height: 600 });
  const scroller = new FakeElement({
    clientHeight: 600,
    overflowY: "auto",
    scrollHeight: 2_000,
    width: 800,
    height: 600,
  });
  scroller.parentElement = documentElement;
  scroller.scrollTop = originalScrollTop;
  scroller.style.setProperty("scroll-behavior", "smooth");

  const document = {
    createElement(tagName) {
      const element = tagName === "img" ? new FakeImageElement() : new FakeElement();
      if (tagName === "style") {
        createdStyles.push(element);
      }
      return element;
    },
    documentElement,
    elementFromPoint() {
      return currentVisibleElements()[0] || scroller;
    },
    elementsFromPoint() {
      const elements = currentVisibleElements();
      return elements.length > 0 ? elements : [scroller];
    },
    fonts: { ready: Promise.resolve() },
    head: {
      appendChild(element) {
        element.isConnected = true;
      },
    },
    scrollingElement: scroller,
  };

  function fakeSetTimeout(callback, delay, ...args) {
    if (Number(delay) >= 5 * 60 * 1_000) {
      const id = { kind: "watchdog", sequence: ++timerSequence };
      watchdogTimers.set(id, () => callback(...args));
      return id;
    }
    return setTimeout(callback, Math.min(Number(delay) || 0, 2), ...args);
  }

  function fakeClearTimeout(id) {
    if (!watchdogTimers.delete(id)) {
      clearTimeout(id);
    }
  }

  const runtime = {
    onMessage: {
      addListener(listener) {
        messageListeners.push(listener);
      },
    },
  };
  const context = {
    HTMLImageElement: FakeImageElement,
    Element: FakeElement,
    WeakSet,
    chrome: { runtime },
    clearTimeout: fakeClearTimeout,
    console,
    crypto: { randomUUID: () => "capture-test-session" },
    devicePixelRatio,
    document,
    getComputedStyle(element) {
      return { overflowY: element.overflowY, position: element.position };
    },
    innerHeight: 600,
    innerWidth: 800,
    requestAnimationFrame(callback) {
      queueMicrotask(() => callback(Date.now()));
    },
    setTimeout: fakeSetTimeout,
  };
  context.globalThis = context;
  vm.createContext(context);

  for (const source of ["src/shared/scroll-core.js", "src/content/capture-session.js"]) {
    const code = fs.readFileSync(path.join(__dirname, "..", source), "utf8");
    vm.runInContext(code, context, { filename: source });
  }

  async function command(commandName, payload = {}) {
    return new Promise((resolve, reject) => {
      const accepted = messageListeners[0](
        { type: "petaldesk.capture.command", command: commandName, payload },
        null,
        (response) => {
          if (response.ok) {
            resolve(response.result);
          } else {
            reject(new Error(response.error.message));
          }
        },
      );
      assert.equal(accepted, true);
    });
  }

  return {
    command,
    context,
    createdStyles,
    scroller,
    setVisibleElements(elementsOrProvider) {
      visibleElements = elementsOrProvider;
    },
    stickyElement(options = {}) {
      const element = new FakeElement({ position: "sticky", ...options });
      element.parentElement = scroller;
      return element;
    },
    watchdogTimers,
    fireWatchdog() {
      const entry = Array.from(watchdogTimers.entries()).at(-1);
      assert.ok(entry, "an active capture watchdog should exist");
      watchdogTimers.delete(entry[0]);
      entry[1]();
    },
  };
}

test("capture status reports one finite DPR for the entire session", async () => {
  const harness = loadCaptureSession({ devicePixelRatio: 1.75 });
  const idle = await harness.command("status");
  assert.equal(idle.devicePixelRatio, 1.75);

  const prepared = await harness.command("prepare", {
    anchor: { x: 350, y: 175 },
    coordinateSpace: "clientPhysical",
  });
  assert.equal(prepared.devicePixelRatio, 1.75);
  assert.deepEqual(JSON.parse(JSON.stringify(prepared.anchor)), { x: 200, y: 100 });

  harness.context.devicePixelRatio = Number.POSITIVE_INFINITY;
  const active = await harness.command("status");
  assert.equal(active.devicePixelRatio, 1.75);
  const restored = await harness.command("restore");
  assert.equal(restored.devicePixelRatio, 1.75);

  const invalidIdle = await harness.command("status");
  assert.equal(invalidIdle.devicePixelRatio, 1);
});

test("stale session IDs cannot scroll or restore a newer capture session", async () => {
  const harness = loadCaptureSession({ originalScrollTop: 90 });
  const prepared = await harness.command("prepare");
  await harness.command("start", { sessionId: prepared.sessionId });

  await assert.rejects(
    harness.command("step", { sessionId: "stale-session", distancePx: 200 }),
    /no longer active/i,
  );
  assert.equal(harness.scroller.scrollTop, 90);
  await assert.rejects(
    harness.command("restore", { sessionId: "stale-session" }),
    /no longer active/i,
  );
  assert.equal(harness.createdStyles.at(-1).isConnected, true);
  assert.equal((await harness.command("status", { sessionId: prepared.sessionId })).state, "capturing");

  const restored = await harness.command("restore", { sessionId: prepared.sessionId });
  assert.equal(restored.restored, true);
  assert.equal(harness.createdStyles.at(-1).isConnected, false);
});

test("watchdog is renewed by commands and restores page mutations after inactivity", async () => {
  const harness = loadCaptureSession({ originalScrollTop: 160 });
  const sticky = harness.stickyElement();
  sticky.style.setProperty("visibility", "visible");
  harness.setVisibleElements([sticky, harness.scroller]);

  await harness.command("prepare");
  const preparedTimer = Array.from(harness.watchdogTimers.keys())[0];
  assert.ok(preparedTimer);
  await harness.command("start", { from: "top" });
  const startedTimer = Array.from(harness.watchdogTimers.keys())[0];
  assert.notEqual(startedTimer, preparedTimer);
  await harness.command("step", { distancePx: 200 });
  assert.equal(sticky.style.getPropertyValue("visibility"), "hidden");
  assert.equal(harness.scroller.scrollTop, 200);

  await harness.command("status");
  assert.equal(harness.watchdogTimers.size, 1);
  harness.fireWatchdog();
  const idle = await harness.command("status");

  assert.equal(idle.state, "idle");
  assert.equal(harness.watchdogTimers.size, 0);
  assert.equal(harness.scroller.scrollTop, 160);
  assert.equal(harness.scroller.style.getPropertyValue("scroll-behavior"), "smooth");
  assert.equal(sticky.style.getPropertyValue("visibility"), "visible");
  assert.equal(harness.createdStyles.at(-1).isConnected, false);
});

test("each scroll incrementally hides newly visible sticky elements without duplicate saves", async () => {
  const harness = loadCaptureSession({ originalScrollTop: 0 });
  const firstSticky = harness.stickyElement({ top: 10 });
  const laterSticky = harness.stickyElement({ top: 80 });
  harness.setVisibleElements(() =>
    harness.scroller.scrollTop === 0
      ? [firstSticky, harness.scroller]
      : [firstSticky, laterSticky, harness.scroller],
  );

  await harness.command("prepare");
  await harness.command("start");
  const firstStep = await harness.command("step", { distancePx: 200 });

  assert.equal(firstStep.fixedElementsHidden, true);
  assert.equal(firstSticky.style.getPropertyValue("visibility"), "hidden");
  assert.equal(laterSticky.style.getPropertyValue("visibility"), "hidden");
  const firstVisibilityWrites = firstSticky.style.writes.filter(
    (write) => write.action === "set" && write.name === "visibility" && write.value === "hidden",
  ).length;
  const laterVisibilityWrites = laterSticky.style.writes.filter(
    (write) => write.action === "set" && write.name === "visibility" && write.value === "hidden",
  ).length;

  await harness.command("step", { distancePx: 200 });
  assert.equal(
    firstSticky.style.writes.filter(
      (write) => write.action === "set" && write.name === "visibility" && write.value === "hidden",
    ).length,
    firstVisibilityWrites,
  );
  assert.equal(
    laterSticky.style.writes.filter(
      (write) => write.action === "set" && write.name === "visibility" && write.value === "hidden",
    ).length,
    laterVisibilityWrites,
  );

  await harness.command("cancel");
  assert.equal(firstSticky.style.getPropertyValue("visibility"), "");
  assert.equal(laterSticky.style.getPropertyValue("visibility"), "");
  assert.equal(harness.watchdogTimers.size, 0);
});
