import assert from "node:assert/strict";
import test from "node:test";
import { resolveBuildTimestamp } from "./build-macos.mjs";

test("prefers an explicit PetalDesk package timestamp", () => {
  assert.equal(
    resolveBuildTimestamp(
      { PETALDESK_BUILD_TIMESTAMP: "1704067200", SOURCE_DATE_EPOCH: "1700000000" },
      1_800_000_000_000,
    ),
    "1704067200",
  );
});

test("uses SOURCE_DATE_EPOCH for a reproducible package", () => {
  assert.equal(
    resolveBuildTimestamp({ SOURCE_DATE_EPOCH: "1700000000" }, 1_800_000_000_000),
    "1700000000",
  );
});

test("uses the current package start time when no override is present", () => {
  assert.equal(resolveBuildTimestamp({}, 1_704_067_200_999), "1704067200");
});

test("rejects malformed package timestamps", () => {
  assert.throws(
    () => resolveBuildTimestamp({ PETALDESK_BUILD_TIMESTAMP: "2026-08-07" }),
    /Unix timestamp in seconds/,
  );
  assert.throws(
    () => resolveBuildTimestamp({ SOURCE_DATE_EPOCH: "0" }),
    /positive Unix timestamp in seconds/,
  );
});
