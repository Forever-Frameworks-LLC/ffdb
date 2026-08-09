import { describe, expect, it } from "vitest";

import { generateDatabaseTypes } from "./typegen.js";

describe("generateDatabaseTypes", () => {
  it("generates deterministic wire-compatible types from CREATE TABLE SQL", () => {
    const generated = generateDatabaseTypes({
      version: 17,
      tables: [
        {
          name: "todo items",
          rls_enabled: true,
          rls_forced: false,
          sql: `CREATE TABLE "todo items" (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            done INTEGER DEFAULT 0,
            payload BLOB,
            created_at TIMESTAMP NOT NULL,
            amount DECIMAL(10, 2),
            CONSTRAINT title_nonempty CHECK (length(title) > 0)
          )`,
        },
      ],
    });

    expect(generated).toContain("// FFDB schema version: 17");
    expect(generated).toContain("export interface TableTodoItems {");
    expect(generated).toContain('readonly "id": string;');
    expect(generated).toContain('readonly "title": string;');
    expect(generated).toContain('readonly "done": number | null;');
    expect(generated).toContain('readonly "payload": BlobValue | null;');
    expect(generated).toContain('readonly "created_at": number;');
    expect(generated).toContain('readonly "amount": unknown | null;');
    expect(generated).toContain('readonly "todo items": TableTodoItems;');
    expect(generated).not.toContain("title_nonempty");
  });

  it("keeps commas inside defaults and checks within one column definition", () => {
    const generated = generateDatabaseTypes({
      version: 1,
      tables: [{
        name: "messages",
        rls_enabled: false,
        rls_forced: false,
        sql: "CREATE TABLE messages (body TEXT DEFAULT ('a,b'), score REAL CHECK (score IN (1, 2)))",
      }],
    });

    expect(generated.match(/readonly "body"/gu)).toHaveLength(1);
    expect(generated.match(/readonly "score"/gu)).toHaveLength(1);
    expect(generated).toContain('readonly "body": string | null;');
    expect(generated).toContain('readonly "score": number | null;');
  });

  it("falls back to an unknown record when column metadata cannot be recovered", () => {
    const generated = generateDatabaseTypes({
      version: 2,
      tables: [{
        name: "report",
        rls_enabled: false,
        rls_forced: false,
        sql: "CREATE VIEW report AS SELECT 1",
      }],
    });

    expect(generated).toContain("export type TableReport = Readonly<Record<string, unknown>>;");
  });

  it("sorts tables and disambiguates normalized interface names", () => {
    const generated = generateDatabaseTypes({
      version: 3,
      tables: [
        { name: "a-b", sql: "CREATE TABLE [a-b] (value INT)", rls_enabled: false, rls_forced: false },
        { name: "a_b", sql: "CREATE TABLE a_b (value INT)", rls_enabled: false, rls_forced: false },
      ],
    });

    expect(generated).toContain('readonly "a-b": TableAB;');
    expect(generated).toContain('readonly "a_b": TableAB2;');
  });
});
