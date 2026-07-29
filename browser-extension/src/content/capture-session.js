(function installCaptureSession(root) {
  "use strict";

  const core = root.PetalDeskScrollCore;
  const extensionApi = root.browser || root.chrome;
  if (!core || !extensionApi || !extensionApi.runtime) {
    return;
  }

  const MESSAGE_TYPE = "petaldesk.capture.command";
  const DOM_QUIET_MS = 120;
  const DOM_QUIET_TIMEOUT_MS = 1_500;
  const SESSION_WATCHDOG_MS = 5 * 60 * 1_000;
  const POSITION_SAMPLE_TARGET_PX = 96;
  const MAX_POSITION_CANDIDATES = 4_096;
  const FREEZE_STYLE = `
    *, *::before, *::after {
      animation-play-state: paused !important;
      transition-property: none !important;
      transition-duration: 0s !important;
      scroll-behavior: auto !important;
      caret-color: transparent !important;
    }
  `;

  let session = null;
  let commandQueue = Promise.resolve();

  function effectiveDevicePixelRatio() {
    const ratio = Number(root.devicePixelRatio);
    return Number.isFinite(ratio) && ratio > 0 ? core.clamp(ratio, 0.1, 16) : 1;
  }

  function clearSessionWatchdog(targetSession) {
    if (targetSession && targetSession.watchdogTimer != null) {
      clearTimeout(targetSession.watchdogTimer);
      targetSession.watchdogTimer = null;
    }
  }

  function renewSessionWatchdog() {
    if (!session) {
      return;
    }

    const targetSession = session;
    clearSessionWatchdog(targetSession);
    targetSession.watchdogTimer = setTimeout(() => {
      targetSession.watchdogTimer = null;
      const restoreTask = commandQueue.then(() => {
        if (!session || session.id !== targetSession.id) {
          return undefined;
        }
        return restoreCurrentSession("expired");
      });
      commandQueue = restoreTask.catch(() => {});
    }, SESSION_WATCHDOG_MS);
  }

  function createSessionId() {
    if (root.crypto && typeof root.crypto.randomUUID === "function") {
      return root.crypto.randomUUID();
    }
    return `capture-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  }

  function metricsFor(element) {
    const isRoot = element === document.scrollingElement;
    const style = root.getComputedStyle(element);
    return {
      scrollTop: element.scrollTop,
      scrollHeight: element.scrollHeight,
      clientHeight: element.clientHeight,
      overflowY: style.overflowY,
      isRoot,
    };
  }

  function parentElementOrHost(element) {
    if (element.parentElement) {
      return element.parentElement;
    }
    const nodeRoot = typeof element.getRootNode === "function" ? element.getRootNode() : null;
    return nodeRoot && nodeRoot.host instanceof Element ? nodeRoot.host : null;
  }

  function scrollCandidatesAt(anchor) {
    const point = core.normalizeAnchor(anchor, root.innerWidth, root.innerHeight);
    const hitElements = typeof document.elementsFromPoint === "function"
      ? document.elementsFromPoint(point.x, point.y)
      : [document.elementFromPoint(point.x, point.y)].filter(Boolean);
    const rankedCandidates = new Map();
    const rootScroller = document.scrollingElement;

    for (let hitIndex = 0; hitIndex < hitElements.length; hitIndex += 1) {
      let element = hitElements[hitIndex];
      let depth = 0;
      while (element instanceof Element && element !== rootScroller) {
        const previous = rankedCandidates.get(element);
        if (
          !previous ||
          depth < previous.depth ||
          (depth === previous.depth && hitIndex < previous.hitIndex)
        ) {
          rankedCandidates.set(element, {
            element,
            metrics: metricsFor(element),
            depth,
            hitIndex,
          });
        }
        element = parentElementOrHost(element);
        depth += 1;
      }
    }

    const candidates = Array.from(rankedCandidates.values()).sort(
      (left, right) => left.depth - right.depth || left.hitIndex - right.hitIndex,
    );
    if (rootScroller) {
      candidates.push({ element: rootScroller, metrics: metricsFor(rootScroller) });
    }

    return { point, candidates };
  }

  function findScrollContainer(anchor) {
    const { point, candidates } = scrollCandidatesAt(anchor);
    const selected = core.selectNearestVerticalScrollContainer(candidates);
    if (!selected) {
      throw new Error("No vertically scrollable container was found at the anchor");
    }
    return { point, element: selected.element };
  }

  function anchorForPayload(payload, devicePixelRatio = effectiveDevicePixelRatio()) {
    const anchor = payload && payload.anchor && typeof payload.anchor === "object"
      ? payload.anchor
      : {};
    if (payload && payload.coordinateSpace === "clientPhysical") {
      return core.normalizeClientPhysicalAnchor(
        anchor,
        devicePixelRatio,
        root.innerWidth,
        root.innerHeight,
      );
    }
    return anchor;
  }

  function saveInlineProperty(element, name) {
    return {
      value: element.style.getPropertyValue(name),
      priority: element.style.getPropertyPriority(name),
    };
  }

  function restoreInlineProperty(element, name, saved) {
    if (!saved || saved.value === "") {
      element.style.removeProperty(name);
      return;
    }
    element.style.setProperty(name, saved.value, saved.priority);
  }

  function injectFreezeStyle() {
    const style = document.createElement("style");
    style.dataset.petaldeskLongCapture = "true";
    style.textContent = FREEZE_STYLE;
    (document.head || document.documentElement).appendChild(style);
    return style;
  }

  function nextAnimationFrame() {
    return new Promise((resolve) => {
      let settled = false;
      const timeout = setTimeout(() => {
        if (!settled) {
          settled = true;
          resolve();
        }
      }, 100);
      root.requestAnimationFrame(() => {
        if (!settled) {
          settled = true;
          clearTimeout(timeout);
          resolve();
        }
      });
    });
  }

  async function waitTwoFrames() {
    await nextAnimationFrame();
    await nextAnimationFrame();
  }

  function waitForDomQuiet(quietMs = DOM_QUIET_MS, timeoutMs = DOM_QUIET_TIMEOUT_MS) {
    if (typeof MutationObserver !== "function") {
      return new Promise((resolve) => setTimeout(resolve, quietMs));
    }

    return new Promise((resolve) => {
      let quietTimer;
      let timeoutTimer;
      const observer = new MutationObserver(() => {
        clearTimeout(quietTimer);
        quietTimer = setTimeout(finish, quietMs);
      });

      function finish() {
        observer.disconnect();
        clearTimeout(quietTimer);
        clearTimeout(timeoutTimer);
        resolve();
      }

      observer.observe(document.documentElement, {
        attributes: true,
        characterData: true,
        childList: true,
        subtree: true,
      });
      quietTimer = setTimeout(finish, quietMs);
      timeoutTimer = setTimeout(finish, timeoutMs);
    });
  }

  function timeoutPromise(timeoutMs) {
    return new Promise((resolve) => setTimeout(resolve, timeoutMs));
  }

  function waitForImage(image, timeoutMs) {
    return new Promise((resolve) => {
      let timer;
      const finish = () => {
        clearTimeout(timer);
        image.removeEventListener("load", finish);
        image.removeEventListener("error", finish);
        resolve();
      };
      image.addEventListener("load", finish, { once: true });
      image.addEventListener("error", finish, { once: true });
      timer = setTimeout(finish, timeoutMs);
    });
  }

  async function waitForVisibleResources(timeoutMs) {
    const pendingImages = Array.from(sampledVisibleElements()).filter(
      (element) => element instanceof HTMLImageElement && !element.complete,
    );
    const imageTasks = pendingImages.map((image) => waitForImage(image, timeoutMs));
    const fontTask = document.fonts && document.fonts.ready
      ? Promise.resolve(document.fonts.ready).catch(() => undefined)
      : Promise.resolve();
    await Promise.all([
      ...imageTasks,
      Promise.race([fontTask, timeoutPromise(timeoutMs)]),
    ]);
  }

  async function waitForSettled(payload = {}) {
    const suppliedQuietMs = Number(payload.domQuietMs);
    const quietMs = Number.isFinite(suppliedQuietMs)
      ? core.clamp(suppliedQuietMs, 40, 500)
      : DOM_QUIET_MS;
    const suppliedTimeoutMs = Number(payload.domQuietTimeoutMs);
    const timeoutMs = Number.isFinite(suppliedTimeoutMs)
      ? core.clamp(suppliedTimeoutMs, quietMs, 5_000)
      : Math.max(quietMs, DOM_QUIET_TIMEOUT_MS);
    await waitTwoFrames();
    await waitForDomQuiet(quietMs, timeoutMs);
    await waitForVisibleResources(Math.min(1_200, timeoutMs));
    await waitTwoFrames();
  }

  function sampledVisibleElements() {
    const width = Math.max(1, root.innerWidth);
    const height = Math.max(1, root.innerHeight);
    const columns = Math.max(3, Math.min(24, Math.ceil(width / POSITION_SAMPLE_TARGET_PX)));
    const rows = Math.max(3, Math.min(18, Math.ceil(height / POSITION_SAMPLE_TARGET_PX)));
    const candidates = new Set();

    for (let row = 0; row <= rows && candidates.size < MAX_POSITION_CANDIDATES; row += 1) {
      const y = core.clamp((row / rows) * (height - 1), 0, height - 1);
      for (let column = 0; column <= columns; column += 1) {
        const x = core.clamp((column / columns) * (width - 1), 0, width - 1);
        const elements = typeof document.elementsFromPoint === "function"
          ? document.elementsFromPoint(x, y)
          : [document.elementFromPoint(x, y)].filter(Boolean);
        for (const hit of elements) {
          let element = hit;
          while (element instanceof Element && candidates.size < MAX_POSITION_CANDIDATES) {
            candidates.add(element);
            element = parentElementOrHost(element);
          }
        }
      }
    }
    return candidates;
  }

  function isInsideViewport(element) {
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0 && rect.right > 0 && rect.bottom > 0
      && rect.left < root.innerWidth && rect.top < root.innerHeight;
  }

  function containsCaptureScroller(element) {
    let current = session && session.element;
    while (current instanceof Element) {
      if (current === element) return true;
      current = parentElementOrHost(current);
    }
    return false;
  }

  function hideVisibleFixedAndStickyElements() {
    if (!session) {
      return 0;
    }

    // Sampling the visible stacking tree avoids an O(total DOM nodes) style
    // walk on very long pages and, importantly, does not hide sticky content
    // that has not entered the viewport yet.
    let hiddenCount = 0;
    for (const element of sampledVisibleElements()) {
      if (
        session.hiddenElementSet.has(element)
        || !isInsideViewport(element)
        || containsCaptureScroller(element)
      ) {
        continue;
      }
      const position = root.getComputedStyle(element).position;
      if (position !== "fixed" && position !== "sticky") {
        continue;
      }
      session.hiddenElements.push({
        element,
        visibility: saveInlineProperty(element, "visibility"),
      });
      session.hiddenElementSet.add(element);
      element.style.setProperty("visibility", "hidden", "important");
      hiddenCount += 1;
    }
    session.fixedElementsHidden = true;
    return hiddenCount;
  }

  function publicStatus(extra = {}) {
    if (!session) {
      return { state: "idle", devicePixelRatio: effectiveDevicePixelRatio(), ...extra };
    }

    return {
      sessionId: session.id,
      state: session.state,
      anchor: session.anchor,
      fixedElementsHidden: session.fixedElementsHidden,
      firstFrameReady: session.firstFrameReady,
      devicePixelRatio: session.devicePixelRatio,
      ...core.createStatus(metricsFor(session.element)),
      ...extra,
    };
  }

  async function restoreCurrentSession(finalState) {
    if (!session) {
      return { state: "idle", restored: false };
    }

    const current = session;
    clearSessionWatchdog(current);
    current.state = finalState;

    for (const hidden of current.hiddenElements) {
      restoreInlineProperty(hidden.element, "visibility", hidden.visibility);
    }
    current.hiddenElements.length = 0;

    current.element.scrollTop = current.originalScrollTop;
    await waitTwoFrames();
    restoreInlineProperty(
      current.element,
      "scroll-behavior",
      current.originalScrollBehavior,
    );
    if (current.freezeStyle && current.freezeStyle.isConnected) {
      current.freezeStyle.remove();
    }

    const result = {
      sessionId: current.id,
      state: finalState,
      restored: true,
      scrollTop: current.element.scrollTop,
      devicePixelRatio: current.devicePixelRatio,
    };
    session = null;
    return result;
  }

  async function prepare(payload = {}) {
    if (session) {
      await restoreCurrentSession("superseded");
    }

    const devicePixelRatio = effectiveDevicePixelRatio();
    const target = findScrollContainer(anchorForPayload(payload, devicePixelRatio));
    const originalScrollBehavior = saveInlineProperty(target.element, "scroll-behavior");
    session = {
      id: createSessionId(),
      state: "prepared",
      anchor: target.point,
      devicePixelRatio,
      element: target.element,
      originalScrollTop: target.element.scrollTop,
      originalScrollBehavior,
      freezeStyle: null,
      fixedElementsHidden: false,
      firstFrameReady: false,
      hiddenElements: [],
      hiddenElementSet: new WeakSet(),
      watchdogTimer: null,
    };
    renewSessionWatchdog();
    target.element.style.setProperty("scroll-behavior", "auto", "important");
    session.freezeStyle = injectFreezeStyle();
    await waitForSettled(payload);
    return publicStatus({ prepared: true });
  }

  async function start(payload = {}) {
    if (!session) {
      throw new Error("Capture must be prepared before it can start");
    }
    if (session.state !== "prepared") {
      throw new Error(`Capture cannot start from state ${session.state}`);
    }

    session.state = "capturing";
    if (payload.from === "top") {
      session.element.scrollTop = 0;
    }
    await waitForSettled(payload);
    session.firstFrameReady = true;
    return publicStatus({ started: true });
  }

  async function step(payload = {}) {
    if (!session || session.state !== "capturing") {
      throw new Error("Capture must be started before it can step");
    }

    const before = metricsFor(session.element);
    if (core.isAtBottom(before, payload.bottomTolerancePx)) {
      return publicStatus({ moved: false, actualDistance: 0 });
    }

    if (session.firstFrameReady) {
      hideVisibleFixedAndStickyElements();
    }

    const plan = core.resolveStep(before, payload.distancePx, payload.stepRatio);
    session.element.scrollTop = plan.targetScrollTop;
    await waitForSettled(payload);
    const actualScrollTop = session.element.scrollTop;
    const newlyHiddenCount = hideVisibleFixedAndStickyElements();
    if (newlyHiddenCount > 0) {
      await waitTwoFrames();
    }

    return publicStatus({
      moved: Math.abs(actualScrollTop - plan.previousScrollTop) > 0.5,
      previousScrollTop: plan.previousScrollTop,
      targetScrollTop: plan.targetScrollTop,
      actualDistance: actualScrollTop - plan.previousScrollTop,
    });
  }

  function validateCommandSession(command, payload) {
    if (command === "prepare" || !payload || payload.sessionId == null) {
      return;
    }
    const requestedSessionId = String(payload.sessionId);
    if (!session || session.id !== requestedSessionId) {
      throw new Error(`Capture session ${requestedSessionId} is no longer active`);
    }
  }

  async function handleCommand(command, payload) {
    validateCommandSession(command, payload);
    switch (command) {
      case "prepare":
        return prepare(payload);
      case "start":
        return start(payload);
      case "step":
        return step(payload);
      case "status":
        return publicStatus();
      case "restore":
        return restoreCurrentSession("restored");
      case "cancel":
        return restoreCurrentSession("cancelled");
      default:
        throw new Error(`Unsupported capture command: ${String(command)}`);
    }
  }

  extensionApi.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    if (!message || message.type !== MESSAGE_TYPE) {
      return false;
    }

    renewSessionWatchdog();
    const commandTask = commandQueue.then(() =>
      handleCommand(message.command, message.payload || {}),
    );
    commandQueue = commandTask.catch(() => {});
    commandTask.then(
      (result) => sendResponse({ ok: true, result }),
      (error) =>
        sendResponse({
          ok: false,
          error: {
            code: "CAPTURE_SESSION_FAILED",
            message: error instanceof Error ? error.message : String(error),
          },
        }),
    );
    return true;
  });
})(typeof globalThis !== "undefined" ? globalThis : this);
