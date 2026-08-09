import { describe, expect, it } from "vitest";

import { parseMigration } from "./migration.js";

describe("parseMigration", () => {
  it("requires explicit up/down SQL and produces the protocol checksum", () => {
    const migration = parseMigration(
      "-- migrate:up\nCREATE TABLE notes (id TEXT);\n-- migrate:down\nDROP TABLE notes;\n",
      "20260802_create_notes.sql",
      100,
    );
    expect(migration.id).toBe("20260802");
    expect(migration.name).toBe("create notes");
    expect(migration.checksum).toMatch(/^[a-f0-9]{64}$/);
  });

  it("rejects missing rollback SQL", () => {
    expect(() => parseMigration("-- migrate:up\nSELECT 1", "1_bad.sql", 100)).toThrow("migrate:down");
  });
});
