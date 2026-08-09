import { DatabaseSync } from "node:sqlite";

import type { JsonValue, SnapshotResponse } from "@ffdb/client";

import {
  applyOptimisticMutation,
  sameMutationContent,
  type PendingMutation,
  type RejectedMutation,
  type ReplicaAdapter,
  type ReplicaRecord,
  type ReplicaTransaction,
} from "./index.js";

const SETUP = `
CREATE TABLE IF NOT EXISTS __ffdb_client_meta (
  key TEXT PRIMARY KEY NOT NULL,
  value TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS __ffdb_client_rows (
  table_name TEXT NOT NULL,
  primary_key_json TEXT NOT NULL,
  values_json TEXT NOT NULL,
  row_version INTEGER NOT NULL,
  server_sequence INTEGER NOT NULL,
  PRIMARY KEY (table_name, primary_key_json)
) STRICT;
CREATE TABLE IF NOT EXISTS __ffdb_client_pending (
  mutation_id TEXT PRIMARY KEY NOT NULL,
  payload_json TEXT NOT NULL,
  enqueued_at_ms INTEGER NOT NULL,
  attempts INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS __ffdb_client_pending_order
  ON __ffdb_client_pending (enqueued_at_ms, mutation_id);
CREATE TABLE IF NOT EXISTS __ffdb_client_rejected (
  mutation_id TEXT PRIMARY KEY NOT NULL,
  payload_json TEXT NOT NULL,
  error_code TEXT NOT NULL,
  rejected_at_ms INTEGER NOT NULL
) STRICT;
`;

interface MetaRow {
  readonly key: string;
  readonly value: string;
}

interface PayloadRow {
  readonly payload_json: string;
}

interface ReplicaRow {
  readonly primary_key_json: string;
  readonly values_json: string;
  readonly row_version: number;
  readonly server_sequence: number;
}

interface RejectedRow extends PayloadRow {
  readonly error_code: string;
  readonly rejected_at_ms: number;
}

/** Durable Node.js 24+ replica using the built-in SQLite driver. */
export class NodeSQLiteReplica implements ReplicaAdapter {
  readonly #database: DatabaseSync;
  readonly #now: () => number;
  #closed = false;
  #writeTail: Promise<void> = Promise.resolve();

  constructor(path: string, now: () => number = Date.now) {
    if (path.trim().length === 0) throw new TypeError("SQLite path is required");
    this.#database = new DatabaseSync(path);
    this.#database.exec("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;");
    this.#database.exec(SETUP);
    this.#now = now;
  }

  transaction<T>(work: (transaction: ReplicaTransaction) => Promise<T>): Promise<T> {
    return this.#write(async () => {
      this.#database.exec("BEGIN IMMEDIATE");
      try {
        const result = await work(new NodeSQLiteReplicaTransaction(this.#database, this.#now));
        this.#database.exec("COMMIT");
        return result;
      } catch (error) {
        this.#database.exec("ROLLBACK");
        throw error;
      }
    });
  }

  async getCursor(): Promise<{ readonly cursor: string; readonly schemaVersion: number } | null> {
    this.#assertOpen();
    const rows = this.#database.prepare(
      "SELECT key, value FROM __ffdb_client_meta WHERE key IN ('cursor', 'schema_version')",
    ).all() as unknown as readonly MetaRow[];
    const entries = Object.fromEntries(rows.map((row) => [row.key, row.value]));
    if (entries.cursor === undefined || entries.schema_version === undefined) return null;
    const schemaVersion = Number(entries.schema_version);
    if (!Number.isSafeInteger(schemaVersion) || schemaVersion < 0) {
      throw new Error("Persisted FFDB schema version is invalid");
    }
    return { cursor: entries.cursor, schemaVersion };
  }

  async getPending(limit: number): Promise<readonly PendingMutation[]> {
    this.#assertOpen();
    validateLimit(limit);
    const rows = this.#database.prepare(
      `SELECT payload_json FROM __ffdb_client_pending
       ORDER BY enqueued_at_ms, mutation_id LIMIT ?`,
    ).all(limit) as unknown as readonly PayloadRow[];
    return rows.map((row) => parsePending(row.payload_json));
  }

  async getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null> {
    await this.#writeTail;
    this.#assertOpen();
    const row = this.#database.prepare(
      `SELECT primary_key_json, values_json, row_version, server_sequence
       FROM __ffdb_client_rows WHERE table_name = ? AND primary_key_json = ?`,
    ).get(table, stableJson(primaryKey)) as unknown as ReplicaRow | undefined;
    return row === undefined ? null : parseReplicaRow(table, row);
  }

  async listRows(table: string): Promise<readonly ReplicaRecord[]> {
    await this.#writeTail;
    this.#assertOpen();
    const rows = this.#database.prepare(
      `SELECT primary_key_json, values_json, row_version, server_sequence
       FROM __ffdb_client_rows WHERE table_name = ? ORDER BY primary_key_json`,
    ).all(table) as unknown as readonly ReplicaRow[];
    return rows.map((row) => parseReplicaRow(table, row));
  }

  async getRejected(limit: number): Promise<readonly RejectedMutation[]> {
    await this.#writeTail;
    this.#assertOpen();
    validateLimit(limit);
    const rows = this.#database.prepare(
      `SELECT payload_json, error_code, rejected_at_ms FROM __ffdb_client_rejected
       ORDER BY rejected_at_ms, mutation_id LIMIT ?`,
    ).all(limit) as unknown as readonly RejectedRow[];
    return rows.map((row) => ({
      ...parsePending(row.payload_json),
      errorCode: row.error_code,
      rejectedAtMs: row.rejected_at_ms,
    }));
  }

  enqueue(mutation: PendingMutation): Promise<void> {
    return this.#write(async () => {
      this.#database.exec("BEGIN IMMEDIATE");
      try {
        const existing = this.#database.prepare(
          "SELECT payload_json FROM __ffdb_client_pending WHERE mutation_id = ?",
        ).get(mutation.mutation_id) as unknown as PayloadRow | undefined;
        const payload = stableJson(mutation);
        if (existing !== undefined) {
          if (!sameMutationContent(parsePending(existing.payload_json), mutation)) {
            throw new Error(`Mutation id ${mutation.mutation_id} was reused`);
          }
        } else {
          const rejected = this.#database.prepare(
            "SELECT payload_json FROM __ffdb_client_rejected WHERE mutation_id = ?",
          ).get(mutation.mutation_id) as unknown as PayloadRow | undefined;
          if (rejected !== undefined) {
            if (!sameMutationContent(parsePending(rejected.payload_json), mutation)) {
              throw new Error(`Mutation id ${mutation.mutation_id} was reused`);
            }
          } else {
            this.#database.prepare(
              `INSERT INTO __ffdb_client_pending
               (mutation_id, payload_json, enqueued_at_ms, attempts) VALUES (?, ?, ?, ?)`,
            ).run(mutation.mutation_id, payload, mutation.enqueuedAtMs, mutation.attempts);
            await applyOptimisticMutation(
              new NodeSQLiteReplicaTransaction(this.#database, this.#now),
              mutation,
            );
          }
        }
        this.#database.exec("COMMIT");
      } catch (error) {
        this.#database.exec("ROLLBACK");
        throw error;
      }
    });
  }

  async close(): Promise<void> {
    await this.#writeTail;
    if (!this.#closed) {
      this.#closed = true;
      this.#database.close();
    }
  }

  #write<T>(work: () => Promise<T>): Promise<T> {
    const result = this.#writeTail.then(async () => {
      this.#assertOpen();
      return work();
    });
    this.#writeTail = result.then(() => undefined, () => undefined);
    return result;
  }

  #assertOpen(): void {
    if (this.#closed) throw new Error("NodeSQLiteReplica is closed");
  }
}

class NodeSQLiteReplicaTransaction implements ReplicaTransaction {
  constructor(
    private readonly database: DatabaseSync,
    private readonly now: () => number,
  ) {}

  async getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null> {
    const row = this.database.prepare(
      `SELECT primary_key_json, values_json, row_version, server_sequence
       FROM __ffdb_client_rows WHERE table_name = ? AND primary_key_json = ?`,
    ).get(table, stableJson(primaryKey)) as unknown as ReplicaRow | undefined;
    return row === undefined ? null : parseReplicaRow(table, row);
  }

  async getPending(limit: number): Promise<readonly PendingMutation[]> {
    validateLimit(limit);
    const rows = this.database.prepare(
      `SELECT payload_json FROM __ffdb_client_pending
       ORDER BY enqueued_at_ms, mutation_id LIMIT ?`,
    ).all(limit) as unknown as readonly PayloadRow[];
    return rows.map((row) => parsePending(row.payload_json));
  }

  async upsert(record: ReplicaRecord): Promise<void> {
    this.database.prepare(
      `INSERT INTO __ffdb_client_rows
       (table_name, primary_key_json, values_json, row_version, server_sequence)
       VALUES (?, ?, ?, ?, ?)
       ON CONFLICT (table_name, primary_key_json) DO UPDATE SET
         values_json = excluded.values_json,
         row_version = excluded.row_version,
         server_sequence = excluded.server_sequence
       WHERE excluded.server_sequence >= __ffdb_client_rows.server_sequence`,
    ).run(
      record.table,
      stableJson(record.primaryKey),
      stableJson(record.values),
      record.rowVersion,
      record.serverSequence,
    );
  }

  async delete(table: string, primaryKey: JsonValue, _rowVersion: number, serverSequence: number): Promise<void> {
    this.database.prepare(
      `DELETE FROM __ffdb_client_rows
       WHERE table_name = ? AND primary_key_json = ? AND server_sequence <= ?`,
    ).run(table, stableJson(primaryKey), serverSequence);
  }

  async replaceSnapshot(snapshot: SnapshotResponse): Promise<void> {
    this.database.exec("DELETE FROM __ffdb_client_rows");
    for (const [table, result] of Object.entries(snapshot.tables)) {
      for (const record of decodeSnapshotTable(table, result)) await this.upsert(record);
    }
  }

  async setCursor(cursor: string, schemaVersion: number): Promise<void> {
    const statement = this.database.prepare(
      `INSERT INTO __ffdb_client_meta (key, value) VALUES (?, ?)
       ON CONFLICT (key) DO UPDATE SET value = excluded.value`,
    );
    statement.run("cursor", cursor);
    statement.run("schema_version", String(schemaVersion));
  }

  async clearCursor(): Promise<void> {
    this.database.exec("DELETE FROM __ffdb_client_meta WHERE key IN ('cursor', 'schema_version')");
  }

  async removePending(mutationIds: readonly string[]): Promise<void> {
    const statement = this.database.prepare("DELETE FROM __ffdb_client_pending WHERE mutation_id = ?");
    for (const id of mutationIds) statement.run(id);
  }

  async rejectPending(mutationId: string, errorCode: string): Promise<void> {
    const row = this.database.prepare(
      "SELECT payload_json FROM __ffdb_client_pending WHERE mutation_id = ?",
    ).get(mutationId) as unknown as PayloadRow | undefined;
    if (row === undefined) return;
    this.database.prepare(
      `INSERT INTO __ffdb_client_rejected
       (mutation_id, payload_json, error_code, rejected_at_ms) VALUES (?, ?, ?, ?)
       ON CONFLICT (mutation_id) DO UPDATE SET
         payload_json = excluded.payload_json,
         error_code = excluded.error_code,
         rejected_at_ms = excluded.rejected_at_ms`,
    ).run(mutationId, row.payload_json, errorCode, this.now());
    await this.removePending([mutationId]);
  }
}

function decodeSnapshotTable(table: string, result: SnapshotResponse["tables"][string]): readonly ReplicaRecord[] {
  const names = result.columns.map((column) => column.name);
  const keyIndex = names.indexOf("__ffdb_primary_key");
  const versionIndex = names.indexOf("__ffdb_row_version");
  const sequenceIndex = names.indexOf("__ffdb_server_sequence");
  if (keyIndex < 0 || versionIndex < 0 || sequenceIndex < 0) return [];
  return result.rows.map((row) => {
    const encodedKey = row[keyIndex];
    if (typeof encodedKey !== "string") throw new Error("Snapshot primary key is invalid");
    let primaryKey: JsonValue;
    try {
      primaryKey = JSON.parse(encodedKey) as JsonValue;
    } catch {
      throw new Error("Snapshot primary key is invalid");
    }
    const values: Record<string, JsonValue> = {};
    names.forEach((name, index) => {
      const value = row[index];
      if (!name.startsWith("__ffdb_") && value !== undefined) values[name] = value as JsonValue;
    });
    return {
      table,
      primaryKey,
      values,
      rowVersion: Number(row[versionIndex]),
      serverSequence: Number(row[sequenceIndex]),
    };
  });
}

function parsePending(encoded: string): PendingMutation {
  const value = JSON.parse(encoded) as PendingMutation;
  if (typeof value.mutation_id !== "string") throw new Error("Persisted pending mutation is invalid");
  return value;
}

function parseReplicaRow(table: string, row: ReplicaRow): ReplicaRecord {
  return {
    table,
    primaryKey: JSON.parse(row.primary_key_json) as JsonValue,
    values: JSON.parse(row.values_json) as Readonly<Record<string, JsonValue>>,
    rowVersion: row.row_version,
    serverSequence: row.server_sequence,
  };
}

function stableJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  return `{${Object.entries(value as Record<string, unknown>)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, entry]) => `${JSON.stringify(key)}:${stableJson(entry)}`)
    .join(",")}}`;
}

function validateLimit(limit: number): void {
  if (!Number.isSafeInteger(limit) || limit < 1) throw new RangeError("limit must be a positive integer");
}
