import { describe, expect, it } from "vitest";

import { generateId } from "./id.js";

describe("generateId", () => {
  it("creates prefixed cryptographically random UUIDs", () => {
    const first = generateId("todo_");
    const second = generateId("todo_");
    expect(first).toMatch(/^todo_[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u);
    expect(second).not.toBe(first);
  });

  it("rejects prefixes that could be confused with structured data", () => {
    expect(() => generateId("../../row/")).toThrow("ID prefix");
  });
});
