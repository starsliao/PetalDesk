import { describe, expect, it } from "vitest";
import { commitHistory, createHistory, redoHistory, undoHistory } from "./history";

describe("screenshot command history", () => {
  it("undoes and redoes one completed action at a time", () => {
    let history = createHistory<string[]>([]);
    history = commitHistory(history, ["rectangle"]);
    history = commitHistory(history, ["rectangle", "arrow"]);
    expect(undoHistory(history).present).toEqual(["rectangle"]);
    history = undoHistory(history);
    expect(redoHistory(history).present).toEqual(["rectangle", "arrow"]);
  });

  it("drops the redo branch when a new action is committed", () => {
    let history = createHistory(0);
    history = commitHistory(history, 1);
    history = commitHistory(history, 2);
    history = undoHistory(history);
    history = commitHistory(history, 3);
    expect(history).toMatchObject({ present: 3, future: [] });
    expect(redoHistory(history)).toBe(history);
  });

  it("bounds retained history for long screenshot sessions", () => {
    let history = createHistory(0);
    for (let value = 1; value <= 12; value += 1) history = commitHistory(history, value, 5);
    expect(history.past).toEqual([7, 8, 9, 10, 11]);
  });
});
