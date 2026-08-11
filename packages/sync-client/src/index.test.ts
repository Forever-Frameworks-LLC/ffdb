import { describe, expect, it } from "vitest";

import type { FFDBClient, LogicalChange, SnapshotResponse } from "@ffdb/client";

import { MemoryReplica, OfflineSyncClient } from "./index.js";

describe("MemoryReplica", () => {
  it("keeps the newest server-sequenced row", async () => {
    const replica = new MemoryReplica();
    await replica.upsert({
      table: "notes",
      primaryKey: "1",
      values: { title: "new" },
      rowVersion: 2,
      serverSequence: 20,
    });
    await replica.upsert({
      table: "notes",
      primaryKey: "1",
      values: { title: "old" },
      rowVersion: 1,
      serverSequence: 10,
    });
    expect(replica.rows()[0]?.values.title).toBe("new");
  });

  it("reads one row and lists a table in deterministic primary-key order", async () => {
    const replica = new MemoryReplica();
    await replica.upsert({
      table: "notes",
      primaryKey: "note-2",
      values: { title: "second" },
      rowVersion: 1,
      serverSequence: 2,
    });
    await replica.upsert({
      table: "notes",
      primaryKey: "note-1",
      values: { title: "first" },
      rowVersion: 1,
      serverSequence: 1,
    });
    await replica.upsert({
      table: "tasks",
      primaryKey: "task-1",
      values: { title: "not a note" },
      rowVersion: 1,
      serverSequence: 3,
    });

    expect(await replica.getRow("notes", "note-1")).toMatchObject({
      table: "notes",
      primaryKey: "note-1",
      values: { title: "first" },
    });
    expect((await replica.listRows("notes")).map((row) => row.primaryKey)).toEqual([
      "note-1",
      "note-2",
    ]);
    expect(await replica.getRow("notes", "missing")).toBeNull();
  });

  it("rolls back an adapter transaction after failure", async () => {
    const replica = new MemoryReplica();
    await expect(
      replica.transaction(async (transaction) => {
        await transaction.upsert({
          table: "notes",
          primaryKey: "1",
          values: { title: "temporary" },
          rowVersion: 1,
          serverSequence: 1,
        });
        throw new Error("stop");
      }),
    ).rejects.toThrow("stop");
    expect(replica.rows()).toEqual([]);
  });
});

describe("OfflineSyncClient", () => {
  it("applies inserts, updates, and deletes optimistically while retaining the queue", async () => {
    let now = 100;
    const replica = new MemoryReplica(() => now);
    const sync = new OfflineSyncClient({} as FFDBClient, replica, { now: () => now++ });

    await sync.mutate({
      mutation_id: "mutation-insert",
      table: "notes",
      primary_key: "note-1",
      operation: "insert",
      values: { id: "note-1", title: "offline", complete: false },
      base_row_version: null,
      client_timestamp_ms: 100,
    });
    expect(await sync.getRow("notes", "note-1")).toMatchObject({
      values: { id: "note-1", title: "offline", complete: false },
      rowVersion: 0,
      serverSequence: -1,
    });
    await sync.mutate({
      mutation_id: "mutation-insert",
      table: "notes",
      primary_key: "note-1",
      operation: "insert",
      values: { id: "note-1", title: "offline", complete: false },
      base_row_version: null,
      client_timestamp_ms: 100,
    });
    expect(await sync.getPending()).toHaveLength(1);

    await sync.mutate({
      mutation_id: "mutation-update",
      table: "notes",
      primary_key: "note-1",
      operation: "update",
      values: { title: "edited" },
      base_row_version: 0,
      client_timestamp_ms: 101,
    });
    expect((await sync.getRow("notes", "note-1"))?.values).toEqual({
      id: "note-1",
      title: "edited",
      complete: false,
    });
    expect(await sync.getPending()).toHaveLength(2);

    await sync.mutate({
      mutation_id: "mutation-delete",
      table: "notes",
      primary_key: "note-1",
      operation: "delete",
      values: null,
      base_row_version: 0,
      client_timestamp_ms: 102,
    });
    expect(await sync.getRow("notes", "note-1")).toBeNull();
    expect(await sync.listRows("notes")).toEqual([]);
    expect(await sync.getPending()).toHaveLength(3);
  });

  it("pulls from the pre-push cursor so an accepted local mutation reaches the replica", async () => {
    const snapshot: SnapshotResponse = { schema_version: 1, cursor: "before-push", tables: {} };
    const change: LogicalChange = {
      sequence: 1,
      transaction_id: "transaction-1",
      table: "notes",
      primary_key: "note-1",
      operation: "insert",
      row_version: 1,
      values: { title: "offline" },
      tombstone: null,
      actor: "user-1",
      schema_version: 1,
      committed_at_ms: 100,
      client_mutation_id: "mutation-1",
    };
    const pullCursors: (string | null)[] = [];
    const client = {
      sync: {
        snapshot: async () => snapshot,
        push: async () => ({
          cursor: "after-push",
          results: [{
            mutation_id: "mutation-1",
            status: "applied" as const,
            server_sequence: 1,
            row_version: 1,
            error_code: null,
          }],
        }),
        pull: async (cursor: string | null) => {
          pullCursors.push(cursor);
          return { changes: [change], cursor: "after-push", has_more: false, control: null };
        },
      },
    } as unknown as FFDBClient;
    const replica = new MemoryReplica();
    const sync = new OfflineSyncClient(client, replica, { now: () => 100 });

    await sync.mutate({
      mutation_id: "mutation-1",
      table: "notes",
      primary_key: "note-1",
      operation: "insert",
      values: { title: "offline" },
      base_row_version: null,
      client_timestamp_ms: 100,
    });
    await sync.sync();

    expect(pullCursors).toEqual(["before-push"]);
    expect(replica.rows()).toEqual([{
      table: "notes",
      primaryKey: "note-1",
      values: { title: "offline" },
      rowVersion: 1,
      serverSequence: 1,
    }]);
    expect(sync.state).toMatchObject({ phase: "idle", pending: 0, error: null });
  });

  it("replaces a scalar snapshot row when an older server sends its update with an object key", async () => {
    const snapshot: SnapshotResponse = {
      schema_version: 1,
      cursor: "snapshot-cursor",
      tables: {
        notes: {
          columns: [
            { name: "id", type: "text" },
            { name: "title", type: "text" },
            { name: "complete", type: "integer" },
            { name: "__ffdb_primary_key", type: "text" },
            { name: "__ffdb_row_version", type: "integer" },
            { name: "__ffdb_server_sequence", type: "integer" },
          ],
          rows: [["note-1", "asdf", 0, '"note-1"', 1, 1]],
          affected_rows: 0,
          last_insert_rowid: null,
          truncated: false,
        },
      },
    };
    const update: LogicalChange = {
      sequence: 2,
      transaction_id: "transaction-2",
      table: "notes",
      primary_key: { id: "note-1" },
      operation: "update",
      row_version: 2,
      values: { id: "note-1", title: "asdf", complete: 1 },
      tombstone: null,
      actor: "user-1",
      schema_version: 1,
      committed_at_ms: 200,
      client_mutation_id: "complete-note-1",
    };
    const client = {
      sync: {
        snapshot: async () => snapshot,
        push: async () => ({ cursor: "snapshot-cursor", results: [] }),
        pull: async () => ({
          changes: [update],
          cursor: "update-cursor",
          has_more: false,
          control: null,
        }),
      },
    } as unknown as FFDBClient;
    const replica = new MemoryReplica();
    const sync = new OfflineSyncClient(client, replica);

    await sync.sync();

    expect(await sync.listRows("notes")).toEqual([{
      table: "notes",
      primaryKey: "note-1",
      values: { id: "note-1", title: "asdf", complete: 1 },
      rowVersion: 2,
      serverSequence: 2,
    }]);
    expect(await sync.getRow("notes", { id: "note-1" })).toBeNull();
  });

  it("rejects an incomplete push response instead of retrying the same pending batch forever", async () => {
    const client = {
      sync: {
        snapshot: async () => ({ schema_version: 1, cursor: "cursor-0", tables: {} }),
        push: async () => ({ cursor: "cursor-1", results: [] }),
        pull: async () => { throw new Error("pull must not run"); },
      },
    } as unknown as FFDBClient;
    const replica = new MemoryReplica();
    const sync = new OfflineSyncClient(client, replica);
    await sync.mutate({
      mutation_id: "mutation-1",
      table: "notes",
      primary_key: "note-1",
      operation: "insert",
      values: { title: "offline" },
      base_row_version: null,
      client_timestamp_ms: null,
    });

    await expect(sync.sync()).rejects.toThrow("exactly one result");
    expect(await replica.getPending(10)).toHaveLength(1);
    expect(sync.state.phase).toBe("error");
  });

  it("restores authoritative rows and retains rejection details after a rejected optimistic mutation", async () => {
    const snapshot: SnapshotResponse = {
      schema_version: 1,
      cursor: "authoritative-cursor",
      tables: {
        notes: {
          columns: [
            { name: "id", type: "text" },
            { name: "title", type: "text" },
            { name: "__ffdb_primary_key", type: "text" },
            { name: "__ffdb_row_version", type: "integer" },
            { name: "__ffdb_server_sequence", type: "integer" },
          ],
          rows: [["note-1", "server title", '"note-1"', 4, 20]],
          affected_rows: 0,
          last_insert_rowid: null,
          truncated: false,
        },
      },
    };
    let snapshotCalls = 0;
    const client = {
      sync: {
        snapshot: async () => {
          snapshotCalls += 1;
          return snapshot;
        },
        push: async () => ({
          cursor: "authoritative-cursor",
          results: [{
            mutation_id: "mutation-rejected",
            status: "rejected" as const,
            server_sequence: null,
            row_version: null,
            error_code: "sync.conflict",
          }],
        }),
        pull: async () => ({
          changes: [],
          cursor: "authoritative-cursor",
          has_more: false,
          control: null,
        }),
      },
    } as unknown as FFDBClient;
    const replica = new MemoryReplica(() => 250);
    const sync = new OfflineSyncClient(client, replica, { now: () => 200 });

    await sync.mutate({
      mutation_id: "mutation-rejected",
      table: "notes",
      primary_key: "note-1",
      operation: "update",
      values: { title: "optimistic title" },
      base_row_version: 3,
      client_timestamp_ms: 200,
    });
    expect((await sync.getRow("notes", "note-1"))?.values.title).toBe("optimistic title");

    await sync.sync();

    expect(snapshotCalls).toBe(2);
    expect((await sync.getRow("notes", "note-1"))?.values.title).toBe("server title");
    expect(await sync.getPending()).toEqual([]);
    expect(await sync.getRejected()).toEqual([expect.objectContaining({
      mutation_id: "mutation-rejected",
      errorCode: "sync.conflict",
      rejectedAtMs: 250,
    })]);
    await sync.mutate({
      mutation_id: "mutation-rejected",
      table: "notes",
      primary_key: "note-1",
      operation: "update",
      values: { title: "optimistic title" },
      base_row_version: 3,
      client_timestamp_ms: 200,
    });
    expect((await sync.getRow("notes", "note-1"))?.values.title).toBe("server title");
    await expect(sync.mutate({
      mutation_id: "mutation-rejected",
      table: "notes",
      primary_key: "note-1",
      operation: "update",
      values: { title: "different content" },
      base_row_version: 3,
      client_timestamp_ms: 200,
    })).rejects.toThrow("was reused");
  });

  it("durably invalidates the cursor when rejection recovery is interrupted", async () => {
    const authoritative: SnapshotResponse = {
      schema_version: 1,
      cursor: "cursor-after-recovery",
      tables: {
        notes: {
          columns: [
            { name: "title", type: "text" },
            { name: "__ffdb_primary_key", type: "text" },
            { name: "__ffdb_row_version", type: "integer" },
            { name: "__ffdb_server_sequence", type: "integer" },
          ],
          rows: [["server title", '"note-1"', 2, 10]],
          affected_rows: 0,
          last_insert_rowid: null,
          truncated: false,
        },
      },
    };
    let snapshotAttempts = 0;
    let pushCalls = 0;
    const client = {
      sync: {
        snapshot: async () => {
          snapshotAttempts += 1;
          if (snapshotAttempts === 1) throw new Error("snapshot network failure");
          return authoritative;
        },
        push: async () => {
          pushCalls += 1;
          return {
            cursor: "cursor-before-recovery",
            results: [{
              mutation_id: "mutation-rejected",
              status: "rejected" as const,
              server_sequence: null,
              row_version: null,
              error_code: "sync.conflict",
            }],
          };
        },
        pull: async () => ({
          changes: [],
          cursor: "cursor-after-recovery",
          has_more: false,
          control: null,
        }),
      },
    } as unknown as FFDBClient;
    const replica = new MemoryReplica(() => 400);
    await replica.transaction(async (transaction) => {
      await transaction.upsert({
        table: "notes",
        primaryKey: "note-1",
        values: { title: "server title" },
        rowVersion: 2,
        serverSequence: 10,
      });
      await transaction.setCursor("cursor-before-recovery", 1);
    });
    const sync = new OfflineSyncClient(client, replica, { now: () => 300 });
    await sync.mutate({
      mutation_id: "mutation-rejected",
      table: "notes",
      primary_key: "note-1",
      operation: "update",
      values: { title: "optimistic title" },
      base_row_version: 2,
      client_timestamp_ms: 300,
    });

    await expect(sync.sync()).rejects.toThrow("snapshot network failure");
    expect(await replica.getCursor()).toBeNull();
    expect((await sync.getRow("notes", "note-1"))?.values.title).toBe("optimistic title");

    await sync.sync();

    expect(pushCalls).toBe(1);
    expect(snapshotAttempts).toBe(2);
    expect((await sync.getRow("notes", "note-1"))?.values.title).toBe("server title");
    expect(await sync.getRejected()).toHaveLength(1);
  });
});
