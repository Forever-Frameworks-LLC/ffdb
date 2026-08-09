import { describe, expect, it } from "vitest";

import type { SnapshotResponse } from "@ffdb/client";

import {
  NativeSQLiteReplica,
  ReactNativeSessionStore,
  type NativeSQLiteDriver,
  type SQLitePrimitive,
  type SQLiteResult,
} from "./index.js";

describe("ReactNativeSessionStore", () => {
  it("removes malformed persisted sessions", async () => {
    const values = new Map([["session", "not json"]]);
    const store = new ReactNativeSessionStore(
      {
        getItem: async (key) => values.get(key) ?? null,
        setItem: async (key, value) => {
          values.set(key, value);
        },
        removeItem: async (key) => {
          values.delete(key);
        },
      },
      "session",
    );
    expect(await store.get()).toBeNull();
    expect(values.has("session")).toBe(false);
  });

  it("removes valid JSON that is not an FFDB session", async () => {
    const values = new Map([["session", "{}"]]);
    const store = new ReactNativeSessionStore(
      {
        getItem: async (key) => values.get(key) ?? null,
        setItem: async (key, value) => { values.set(key, value); },
        removeItem: async (key) => { values.delete(key); },
      },
      "session",
    );

    expect(await store.get()).toBeNull();
    expect(values.has("session")).toBe(false);
  });
});

describe("NativeSQLiteReplica", () => {
  it("can retry initialization after a transient driver failure", async () => {
    let transactions = 0;
    const driver: NativeSQLiteDriver = {
      async execute<Row extends Readonly<Record<string, SQLitePrimitive>>>(): Promise<SQLiteResult<Row>> {
        return { rows: [], changes: 0 };
      },
      async transaction<T>(work: (transactionDriver: NativeSQLiteDriver) => Promise<T>): Promise<T> {
        transactions += 1;
        if (transactions === 1) throw new Error("database temporarily unavailable");
        return work(driver);
      },
    };
    const replica = new NativeSQLiteReplica(driver);

    await expect(replica.initialize()).rejects.toThrow("temporarily unavailable");
    await expect(replica.initialize()).resolves.toBeUndefined();
    expect(transactions).toBe(2);
  });

  it("compares an existing payload inside a transaction without invalid SQLite RAISE SQL", async () => {
    const statements: string[] = [];
    const pending = new Map<string, string>();
    let transactions = 0;
    const driver: NativeSQLiteDriver = {
      async execute<Row extends Readonly<Record<string, SQLitePrimitive>>>(
        sql: string,
        parameters: readonly SQLitePrimitive[] = [],
      ): Promise<SQLiteResult<Row>> {
        statements.push(sql);
        if (sql.startsWith("SELECT payload_json")) {
          const payload = pending.get(String(parameters[0]));
          return {
            rows: (payload === undefined ? [] : [{ payload_json: payload }]) as unknown as readonly Row[],
            changes: 0,
          };
        }
        if (sql.includes("INSERT INTO __ffdb_client_pending")) {
          pending.set(String(parameters[0]), String(parameters[1]));
          return { rows: [], changes: 1 };
        }
        return { rows: [], changes: 0 };
      },
      async transaction<T>(work: (transactionDriver: NativeSQLiteDriver) => Promise<T>): Promise<T> {
        transactions += 1;
        return work(driver);
      },
    };
    const replica = new NativeSQLiteReplica(driver, () => 100);
    const mutation = {
      mutation_id: "mutation-1",
      table: "todos",
      primary_key: "todo-1",
      operation: "insert" as const,
      values: { title: "first" },
      base_row_version: null,
      client_timestamp_ms: 100,
      enqueuedAtMs: 100,
      attempts: 0,
    };
    await replica.enqueue(mutation);
    await replica.enqueue(mutation);
    await expect(replica.enqueue({ ...mutation, values: { title: "changed" } })).rejects.toThrow(
      "mutation id reused",
    );
    expect(transactions).toBeGreaterThanOrEqual(4); // setup plus three atomic enqueue checks
    expect(statements.every((sql) => !sql.includes("RAISE("))).toBe(true);
    expect(statements.filter((sql) => sql.includes("INSERT INTO __ffdb_client_pending"))).toHaveLength(1);
    expect(statements.some((sql) => sql.includes("INSERT INTO __ffdb_client_rows"))).toBe(true);
  });

  it("exposes decoded local row reads and deterministic table lists", async () => {
    const storedRows = [
      {
        primary_key_json: '"note-1"',
        values_json: '{"id":"note-1","title":"first"}',
        row_version: 2,
        server_sequence: 8,
      },
      {
        primary_key_json: '"note-2"',
        values_json: '{"id":"note-2","title":"second"}',
        row_version: 1,
        server_sequence: 9,
      },
    ] as const;
    const driver: NativeSQLiteDriver = {
      async execute<Row extends Readonly<Record<string, SQLitePrimitive>>>(
        sql: string,
        parameters: readonly SQLitePrimitive[] = [],
      ): Promise<SQLiteResult<Row>> {
        if (sql.includes("FROM __ffdb_client_rows WHERE") && parameters.length === 2) {
          const row = storedRows.find((candidate) => candidate.primary_key_json === parameters[1]);
          return { rows: (row === undefined ? [] : [row]) as unknown as readonly Row[], changes: 0 };
        }
        if (sql.includes("FROM __ffdb_client_rows WHERE") && sql.includes("ORDER BY")) {
          return { rows: storedRows as unknown as readonly Row[], changes: 0 };
        }
        return { rows: [], changes: 0 };
      },
      async transaction<T>(work: (transactionDriver: NativeSQLiteDriver) => Promise<T>): Promise<T> {
        return work(driver);
      },
    };
    const replica = new NativeSQLiteReplica(driver);

    expect(await replica.getRow("notes", "note-2")).toEqual({
      table: "notes",
      primaryKey: "note-2",
      values: { id: "note-2", title: "second" },
      rowVersion: 1,
      serverSequence: 9,
    });
    expect((await replica.listRows("notes")).map((row) => row.primaryKey)).toEqual([
      "note-1",
      "note-2",
    ]);
  });

  it("decodes the snapshot primary-key JSON before writing native replica rows", async () => {
    const insertedParameters: Array<readonly SQLitePrimitive[]> = [];
    const driver: NativeSQLiteDriver = {
      async execute<Row extends Readonly<Record<string, SQLitePrimitive>>>(
        sql: string,
        parameters: readonly SQLitePrimitive[] = [],
      ): Promise<SQLiteResult<Row>> {
        if (sql.includes("INSERT INTO __ffdb_client_rows")) insertedParameters.push(parameters);
        return { rows: [], changes: sql.includes("INSERT") ? 1 : 0 };
      },
      async transaction<T>(work: (transactionDriver: NativeSQLiteDriver) => Promise<T>): Promise<T> {
        return work(driver);
      },
    };
    const snapshot: SnapshotResponse = {
      schema_version: 1,
      cursor: "cursor-1",
      tables: {
        notes: {
          columns: [
            { name: "id", type: "text" },
            { name: "title", type: "text" },
            { name: "__ffdb_primary_key", type: "text" },
            { name: "__ffdb_row_version", type: "integer" },
            { name: "__ffdb_server_sequence", type: "integer" },
          ],
          rows: [["note-1", "offline", '"note-1"', 2, 7]],
          affected_rows: 0,
          last_insert_rowid: null,
          truncated: false,
        },
      },
    };
    const replica = new NativeSQLiteReplica(driver);

    await replica.transaction((transaction) => transaction.replaceSnapshot(snapshot));

    expect(insertedParameters).toHaveLength(1);
    expect(insertedParameters[0]?.[1]).toBe('"note-1"');
    expect(insertedParameters[0]?.[2]).toBe('{"id":"note-1","title":"offline"}');
  });
});
