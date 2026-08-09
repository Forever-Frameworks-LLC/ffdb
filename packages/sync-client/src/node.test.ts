import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { NodeSQLiteReplica } from "./node.js";

const directories: string[] = [];

afterEach(async () => {
  await Promise.all(directories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })));
});

describe("NodeSQLiteReplica", () => {
  it("persists cursor, rows, and pending mutations across process-style reopen", async () => {
    const directory = await mkdtemp(join(tmpdir(), "ffdb-node-replica-"));
    directories.push(directory);
    const path = join(directory, "replica.sqlite3");
    const mutation = {
      mutation_id: "mutation-1",
      table: "notes",
      primary_key: "note-1",
      operation: "insert" as const,
      values: { title: "offline" },
      base_row_version: null,
      client_timestamp_ms: 100,
      enqueuedAtMs: 100,
      attempts: 0,
    };

    const first = new NodeSQLiteReplica(path);
    await first.enqueue(mutation);
    expect(await first.getRow("notes", "note-1")).toMatchObject({
      values: { title: "offline" },
      rowVersion: 0,
      serverSequence: -1,
    });
    await first.transaction(async (transaction) => {
      await transaction.upsert({
        table: "notes",
        primaryKey: "note-1",
        values: { title: "offline" },
        rowVersion: 1,
        serverSequence: 1,
      });
      await transaction.setCursor("cursor-1", 2);
    });
    await first.close();

    const reopened = new NodeSQLiteReplica(path);
    expect(await reopened.getCursor()).toEqual({ cursor: "cursor-1", schemaVersion: 2 });
    expect(await reopened.getPending(10)).toEqual([mutation]);
    expect(await reopened.getRow("notes", "note-1")).toMatchObject({
      values: { title: "offline" },
      rowVersion: 1,
      serverSequence: 1,
    });
    expect(await reopened.listRows("notes")).toHaveLength(1);
    await reopened.close();
  });

  it("persists deterministic rejected-mutation records", async () => {
    const directory = await mkdtemp(join(tmpdir(), "ffdb-node-replica-"));
    directories.push(directory);
    const replica = new NodeSQLiteReplica(join(directory, "replica.sqlite3"), () => 500);
    await replica.enqueue({
      mutation_id: "mutation-rejected",
      table: "notes",
      primary_key: "note-1",
      operation: "update",
      values: { title: "offline" },
      base_row_version: 2,
      client_timestamp_ms: 100,
      enqueuedAtMs: 100,
      attempts: 0,
    });
    await replica.transaction((transaction) =>
      transaction.rejectPending("mutation-rejected", "sync.conflict"));

    expect(await replica.getPending(10)).toEqual([]);
    expect(await replica.getRejected(10)).toEqual([expect.objectContaining({
      mutation_id: "mutation-rejected",
      errorCode: "sync.conflict",
      rejectedAtMs: 500,
    })]);
    await replica.close();
  });

  it("rolls back cursor and queue changes when a replica transaction fails", async () => {
    const directory = await mkdtemp(join(tmpdir(), "ffdb-node-replica-"));
    directories.push(directory);
    const replica = new NodeSQLiteReplica(join(directory, "replica.sqlite3"));

    await expect(replica.transaction(async (transaction) => {
      await transaction.setCursor("temporary", 1);
      throw new Error("stop");
    })).rejects.toThrow("stop");
    expect(await replica.getCursor()).toBeNull();
    await replica.close();
  });
});
