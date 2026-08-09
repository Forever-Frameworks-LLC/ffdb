import { describe, expect, it } from "vitest";

import { optimisticList } from "./index.js";

describe("optimisticList", () => {
  it("supports deterministic update and rollback", () => {
    const original = [{ id: "1", title: "before" }] as const;
    const update = optimisticList(original, { id: "1", title: "after" });
    expect(update.next[0]?.title).toBe("after");
    expect(update.rollback()).toBe(original);
  });
});
