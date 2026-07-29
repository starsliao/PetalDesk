const assert = require("node:assert/strict");
const test = require("node:test");

const core = require("../src/shared/scroll-core.js");

test("normalizes an anchor into the viewport", () => {
  assert.deepEqual(core.normalizeAnchor({ x: -20, y: 900 }, 800, 600), {
    x: 0,
    y: 599,
  });
  assert.deepEqual(core.normalizeAnchor(null, 0, 0), { x: 0, y: 0 });
  assert.deepEqual(core.normalizeAnchor(undefined, 801, 601), { x: 400, y: 300 });
});

test("converts client physical pixels using the browser's effective device scale", () => {
  assert.deepEqual(
    core.normalizeClientPhysicalAnchor({ x: 450, y: 300 }, 1.5, 1000, 800),
    { x: 300, y: 200 },
  );
  assert.deepEqual(
    core.normalizeClientPhysicalAnchor({ x: 4000, y: -20 }, 2, 1000, 800),
    { x: 999, y: 0 },
  );
});

test("recognizes vertical scrolling only for eligible overflow modes", () => {
  assert.equal(
    core.isVerticalScrollable({
      scrollHeight: 1_000,
      clientHeight: 500,
      overflowY: "auto",
    }),
    true,
  );
  assert.equal(
    core.isVerticalScrollable({
      scrollHeight: 1_000,
      clientHeight: 500,
      overflowY: "hidden",
    }),
    false,
  );
  assert.equal(
    core.isVerticalScrollable({
      scrollHeight: 1_000,
      clientHeight: 500,
      overflowY: "visible",
      isRoot: true,
    }),
    true,
  );
});

test("selects the nearest eligible candidate", () => {
  const candidates = [
    {
      id: "leaf",
      metrics: { scrollHeight: 300, clientHeight: 300, overflowY: "auto" },
    },
    {
      id: "panel",
      metrics: { scrollHeight: 900, clientHeight: 300, overflowY: "scroll" },
    },
    {
      id: "page",
      metrics: { scrollHeight: 2_000, clientHeight: 700, isRoot: true },
    },
  ];

  assert.equal(core.selectNearestVerticalScrollContainer(candidates).id, "panel");
});

test("calculates and clamps a scroll step", () => {
  assert.deepEqual(
    core.resolveStep(
      { scrollTop: 100, scrollHeight: 1_000, clientHeight: 400 },
      undefined,
    ),
    {
      previousScrollTop: 100,
      targetScrollTop: 360,
      requestedDistance: 260,
      expectedDistance: 260,
    },
  );

  assert.equal(
    core.resolveStep(
      { scrollTop: 550, scrollHeight: 1_000, clientHeight: 400 },
      200,
    ).targetScrollTop,
    600,
  );
});

test("reports bottom state with tolerance and remaining distance", () => {
  const status = core.createStatus({
    scrollTop: 599.4,
    scrollHeight: 1_000,
    clientHeight: 400,
  });

  assert.equal(status.atBottom, true);
  assert.ok(Math.abs(status.remaining - 0.6) < 0.0001);
  assert.equal(
    core.isAtBottom({ scrollTop: 595, scrollHeight: 1_000, clientHeight: 400 }),
    false,
  );
});
