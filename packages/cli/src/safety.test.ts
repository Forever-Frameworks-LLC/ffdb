import { Readable, Writable } from "node:stream";

import { describe, expect, it } from "vitest";

import { confirmDestructive, migrationIdempotencyKey } from "./safety.js";

describe("CLI destructive-operation safety", () => {
  it("requires --yes when stdin is non-interactive", async () => {
    const input = Readable.from([]) as Readable & { isTTY?: boolean };
    const output = new Writable({ write: (_chunk, _encoding, done) => done() }) as Writable & { isTTY?: boolean };
    await expect(confirmDestructive("Restore backup backup-1", false, { input, output }))
      .rejects.toThrow("pass --yes");
    await expect(confirmDestructive("Restore backup backup-1", true, { input, output }))
      .resolves.toBeUndefined();
  });

  it("derives migration replay identity from stable content", () => {
    const migration = {
      id: "20260802",
      name: "create notes",
      up_sql: "CREATE TABLE notes(id TEXT)",
      down_sql: "DROP TABLE notes",
      checksum: "abc123",
      created_at_ms: 1,
    };
    expect(migrationIdempotencyKey(migration)).toBe("migration:20260802:abc123");
    expect(migrationIdempotencyKey({ ...migration, created_at_ms: 999 })).toBe("migration:20260802:abc123");
  });
});
