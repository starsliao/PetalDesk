(function exposeScrollCore(root, factory) {
  const api = factory();

  if (typeof module === "object" && module.exports) {
    module.exports = api;
    return;
  }

  root.PetalDeskScrollCore = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function createScrollCore() {
  "use strict";

  const SCROLL_EPSILON_PX = 1;
  const VERTICAL_SCROLL_OVERFLOWS = new Set(["auto", "scroll", "overlay"]);

  function finiteNumber(value, fallback = 0) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  }

  function clamp(value, minimum, maximum) {
    const low = finiteNumber(minimum);
    const high = Math.max(low, finiteNumber(maximum, low));
    return Math.min(high, Math.max(low, finiteNumber(value, low)));
  }

  function normalizeAnchor(anchor, viewportWidth, viewportHeight) {
    const width = Math.max(1, finiteNumber(viewportWidth, 1));
    const height = Math.max(1, finiteNumber(viewportHeight, 1));
    const source = anchor && typeof anchor === "object" ? anchor : {};
    const x = finiteNumber(source.x, (width - 1) / 2);
    const y = finiteNumber(source.y, (height - 1) / 2);

    return {
      x: clamp(x, 0, Math.max(0, width - 1)),
      y: clamp(y, 0, Math.max(0, height - 1)),
    };
  }

  function normalizeClientPhysicalAnchor(anchor, devicePixelRatio, viewportWidth, viewportHeight) {
    const scale = Math.max(0.1, finiteNumber(devicePixelRatio, 1));
    const source = anchor && typeof anchor === "object" ? anchor : {};
    return normalizeAnchor(
      {
        x: finiteNumber(source.x) / scale,
        y: finiteNumber(source.y) / scale,
      },
      viewportWidth,
      viewportHeight,
    );
  }

  function maxScrollTop(metrics) {
    if (!metrics || typeof metrics !== "object") {
      return 0;
    }

    return Math.max(
      0,
      finiteNumber(metrics.scrollHeight) - finiteNumber(metrics.clientHeight),
    );
  }

  function isVerticalScrollable(metrics, epsilon = SCROLL_EPSILON_PX) {
    if (!metrics || typeof metrics !== "object") {
      return false;
    }

    const hasScrollableRange =
      maxScrollTop(metrics) > Math.max(0, finiteNumber(epsilon, SCROLL_EPSILON_PX));
    if (!hasScrollableRange) {
      return false;
    }

    if (metrics.isRoot) {
      return true;
    }

    return VERTICAL_SCROLL_OVERFLOWS.has(
      String(metrics.overflowY || "").trim().toLowerCase(),
    );
  }

  function selectNearestVerticalScrollContainer(candidates) {
    if (!Array.isArray(candidates)) {
      return null;
    }

    for (const candidate of candidates) {
      if (candidate && isVerticalScrollable(candidate.metrics)) {
        return candidate;
      }
    }

    return null;
  }

  function isAtBottom(metrics, tolerance = SCROLL_EPSILON_PX) {
    const remaining = maxScrollTop(metrics) - finiteNumber(metrics && metrics.scrollTop);
    return remaining <= Math.max(0, finiteNumber(tolerance, SCROLL_EPSILON_PX));
  }

  function resolveStep(metrics, requestedStep, defaultRatio = 0.65) {
    const current = clamp(
      metrics && metrics.scrollTop,
      0,
      maxScrollTop(metrics),
    );
    const ratio = clamp(defaultRatio, 0.05, 1);
    const defaultStep = Math.max(
      1,
      Math.floor(finiteNumber(metrics && metrics.clientHeight, 1) * ratio),
    );
    const requested = finiteNumber(requestedStep, defaultStep);
    const distance = Math.max(1, requested);
    const target = clamp(current + distance, 0, maxScrollTop(metrics));

    return {
      previousScrollTop: current,
      targetScrollTop: target,
      requestedDistance: distance,
      expectedDistance: target - current,
    };
  }

  function createStatus(metrics, tolerance = SCROLL_EPSILON_PX) {
    const maximum = maxScrollTop(metrics);
    const scrollTop = clamp(metrics && metrics.scrollTop, 0, maximum);

    return {
      scrollTop,
      scrollHeight: Math.max(0, finiteNumber(metrics && metrics.scrollHeight)),
      clientHeight: Math.max(0, finiteNumber(metrics && metrics.clientHeight)),
      maxScrollTop: maximum,
      remaining: Math.max(0, maximum - scrollTop),
      atBottom: isAtBottom({ ...metrics, scrollTop }, tolerance),
    };
  }

  return Object.freeze({
    SCROLL_EPSILON_PX,
    clamp,
    createStatus,
    isAtBottom,
    isVerticalScrollable,
    maxScrollTop,
    normalizeAnchor,
    normalizeClientPhysicalAnchor,
    resolveStep,
    selectNearestVerticalScrollContainer,
  });
});
