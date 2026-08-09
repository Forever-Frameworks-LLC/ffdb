import type { AuthTokenPair, JsonValue, SessionStore, SnapshotResponse } from "@ffdb/client";
import {
  applyOptimisticMutation,
  sameMutationContent,
  type PendingMutation,
  type RejectedMutation,
  type ReplicaAdapter,
  type ReplicaRecord,
  type ReplicaTransaction,
} from "@ffdb/sync-client";

export interface AsyncKeyValueStorage {
  getItem(key: string): Promise<string | null>;
  setItem(key: string, value: string): Promise<void>;
  removeItem(key: string): Promise<void>;
}

export class ReactNativeSessionStore implements SessionStore {
  constructor(
    private readonly storage: AsyncKeyValueStorage,
    private readonly key = "@ffdb/session",
  ) {}

  async get(): Promise<AuthTokenPair | null> {
    const encoded = await this.storage.getItem(this.key);
    if (encoded === null) return null;
    try {
      const session: unknown = JSON.parse(encoded);
      if (!isAuthTokenPair(session)) throw new TypeError("Persisted FFDB session is invalid");
      return session;
    } catch {
      await this.storage.removeItem(this.key);
      return null;
    }
  }

  async set(session: AuthTokenPair | null): Promise<void> {
    if (session === null) await this.storage.removeItem(this.key);
    else await this.storage.setItem(this.key, JSON.stringify(session));
  }
}

export type SQLitePrimitive = string | number | null;

export interface SQLiteResult<Row extends Readonly<Record<string, SQLitePrimitive>> = Readonly<Record<string, SQLitePrimitive>>> {
  readonly rows: readonly Row[];
  readonly changes: number;
}

/** Runtime-neutral contract implemented by Expo SQLite or a native SQLite module. */
export interface NativeSQLiteDriver {
  execute<Row extends Readonly<Record<string, SQLitePrimitive>> = Readonly<Record<string, SQLitePrimitive>>>(
    sql: string,
    parameters?: readonly SQLitePrimitive[],
  ): Promise<SQLiteResult<Row>>;
  transaction<T>(work: (driver: NativeSQLiteDriver) => Promise<T>): Promise<T>;
}

const SETUP = [
  `CREATE TABLE IF NOT EXISTS __ffdb_client_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
  ) STRICT`,
  `CREATE TABLE IF NOT EXISTS __ffdb_client_rows (
    table_name TEXT NOT NULL,
    primary_key_json TEXT NOT NULL,
    values_json TEXT NOT NULL,
    row_version INTEGER NOT NULL,
    server_sequence INTEGER NOT NULL,
    PRIMARY KEY (table_name, primary_key_json)
  ) STRICT`,
  `CREATE TABLE IF NOT EXISTS __ffdb_client_pending (
    mutation_id TEXT PRIMARY KEY NOT NULL,
    payload_json TEXT NOT NULL,
    enqueued_at_ms INTEGER NOT NULL,
    attempts INTEGER NOT NULL
  ) STRICT`,
  `CREATE TABLE IF NOT EXISTS __ffdb_client_rejected (
    mutation_id TEXT PRIMARY KEY NOT NULL,
    payload_json TEXT NOT NULL,
    error_code TEXT NOT NULL,
    rejected_at_ms INTEGER NOT NULL
  ) STRICT`,
] as const;

export class NativeSQLiteReplica implements ReplicaAdapter {
  #ready: Promise<void> | null = null;

  constructor(
    private readonly driver: NativeSQLiteDriver,
    private readonly now: () => number = Date.now,
  ) {}

  initialize(): Promise<void> {
    this.#ready ??= this.driver.transaction(async (driver) => {
      for (const sql of SETUP) await driver.execute(sql);
    }).catch((cause: unknown) => {
      this.#ready = null;
      throw cause;
    });
    return this.#ready;
  }

  async transaction<T>(work: (transaction: ReplicaTransaction) => Promise<T>): Promise<T> {
    await this.initialize();
    return this.driver.transaction((driver) => work(new SQLiteReplicaTransaction(driver, this.now)));
  }

  async getCursor(): Promise<{ readonly cursor: string; readonly schemaVersion: number } | null> {
    await this.initialize();
    const result = await this.driver.execute<{ readonly key: string; readonly value: string }>(
      "SELECT key, value FROM __ffdb_client_meta WHERE key IN ('cursor', 'schema_version')",
    );
    const entries = Object.fromEntries(result.rows.map((row) => [row.key, row.value]));
    if (entries.cursor === undefined || entries.schema_version === undefined) return null;
    return { cursor: entries.cursor, schemaVersion: Number(entries.schema_version) };
  }

  async getPending(limit: number): Promise<readonly PendingMutation[]> {
    await this.initialize();
    validateLimit(limit);
    const result = await this.driver.execute<{ readonly payload_json: string }>(
      "SELECT payload_json FROM __ffdb_client_pending ORDER BY enqueued_at_ms, mutation_id LIMIT ?1",
      [limit],
    );
    return result.rows.map((row) => JSON.parse(row.payload_json) as PendingMutation);
  }

  async getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null> {
    await this.initialize();
    const result = await this.driver.execute<{
      readonly primary_key_json: string;
      readonly values_json: string;
      readonly row_version: number;
      readonly server_sequence: number;
    }>(
      `SELECT primary_key_json, values_json, row_version, server_sequence
       FROM __ffdb_client_rows WHERE table_name = ?1 AND primary_key_json = ?2`,
      [table, stableJson(primaryKey)],
    );
    const row = result.rows[0];
    return row === undefined ? null : decodeReplicaRow(table, row);
  }

  async listRows(table: string): Promise<readonly ReplicaRecord[]> {
    await this.initialize();
    const result = await this.driver.execute<{
      readonly primary_key_json: string;
      readonly values_json: string;
      readonly row_version: number;
      readonly server_sequence: number;
    }>(
      `SELECT primary_key_json, values_json, row_version, server_sequence
       FROM __ffdb_client_rows WHERE table_name = ?1 ORDER BY primary_key_json`,
      [table],
    );
    return result.rows.map((row) => decodeReplicaRow(table, row));
  }

  async getRejected(limit: number): Promise<readonly RejectedMutation[]> {
    await this.initialize();
    validateLimit(limit);
    const result = await this.driver.execute<{
      readonly payload_json: string;
      readonly error_code: string;
      readonly rejected_at_ms: number;
    }>(
      `SELECT payload_json, error_code, rejected_at_ms FROM __ffdb_client_rejected
       ORDER BY rejected_at_ms, mutation_id LIMIT ?1`,
      [limit],
    );
    return result.rows.map((row) => ({
      ...(JSON.parse(row.payload_json) as PendingMutation),
      errorCode: row.error_code,
      rejectedAtMs: row.rejected_at_ms,
    }));
  }

  async enqueue(mutation: PendingMutation): Promise<void> {
    await this.initialize();
    const payload = stableJson(mutation);
    await this.driver.transaction(async (driver) => {
      const existing = await driver.execute<{ readonly payload_json: string }>(
        "SELECT payload_json FROM __ffdb_client_pending WHERE mutation_id = ?1",
        [mutation.mutation_id],
      );
      const row = existing.rows[0];
      if (row !== undefined) {
        if (!sameMutationContent(JSON.parse(row.payload_json) as PendingMutation, mutation)) {
          throw new Error("mutation id reused with different payload");
        }
        return;
      }
      const rejected = await driver.execute<{ readonly payload_json: string }>(
        "SELECT payload_json FROM __ffdb_client_rejected WHERE mutation_id = ?1",
        [mutation.mutation_id],
      );
      const rejectedRow = rejected.rows[0];
      if (rejectedRow !== undefined) {
        if (!sameMutationContent(JSON.parse(rejectedRow.payload_json) as PendingMutation, mutation)) {
          throw new Error("mutation id reused with different payload");
        }
        return;
      }
      await driver.execute(
        `INSERT INTO __ffdb_client_pending (mutation_id, payload_json, enqueued_at_ms, attempts)
         VALUES (?1, ?2, ?3, ?4)`,
        [mutation.mutation_id, payload, mutation.enqueuedAtMs, mutation.attempts],
      );
      await applyOptimisticMutation(new SQLiteReplicaTransaction(driver, this.now), mutation);
    });
  }
}

class SQLiteReplicaTransaction implements ReplicaTransaction {
  constructor(
    private readonly driver: NativeSQLiteDriver,
    private readonly now: () => number,
  ) {}

  async getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null> {
    const result = await this.driver.execute<{
      readonly primary_key_json: string;
      readonly values_json: string;
      readonly row_version: number;
      readonly server_sequence: number;
    }>(
      `SELECT primary_key_json, values_json, row_version, server_sequence
       FROM __ffdb_client_rows WHERE table_name = ?1 AND primary_key_json = ?2`,
      [table, stableJson(primaryKey)],
    );
    const row = result.rows[0];
    return row === undefined ? null : decodeReplicaRow(table, row);
  }

  async getPending(limit: number): Promise<readonly PendingMutation[]> {
    validateLimit(limit);
    const result = await this.driver.execute<{ readonly payload_json: string }>(
      "SELECT payload_json FROM __ffdb_client_pending ORDER BY enqueued_at_ms, mutation_id LIMIT ?1",
      [limit],
    );
    return result.rows.map((row) => JSON.parse(row.payload_json) as PendingMutation);
  }

  async upsert(record: ReplicaRecord): Promise<void> {
    await this.driver.execute(
      `INSERT INTO __ffdb_client_rows
       (table_name, primary_key_json, values_json, row_version, server_sequence)
       VALUES (?1, ?2, ?3, ?4, ?5)
       ON CONFLICT (table_name, primary_key_json) DO UPDATE SET
         values_json = excluded.values_json,
         row_version = excluded.row_version,
         server_sequence = excluded.server_sequence
       WHERE excluded.server_sequence >= __ffdb_client_rows.server_sequence`,
      [
        record.table,
        stableJson(record.primaryKey),
        stableJson(record.values),
        record.rowVersion,
        record.serverSequence,
      ],
    );
  }

  async delete(table: string, primaryKey: JsonValue, _rowVersion: number, serverSequence: number): Promise<void> {
    await this.driver.execute(
      "DELETE FROM __ffdb_client_rows WHERE table_name = ?1 AND primary_key_json = ?2 AND server_sequence <= ?3",
      [table, stableJson(primaryKey), serverSequence],
    );
  }

  async replaceSnapshot(snapshot: SnapshotResponse): Promise<void> {
    await this.driver.execute("DELETE FROM __ffdb_client_rows");
    for (const [table, result] of Object.entries(snapshot.tables)) {
      const names = result.columns.map((column) => column.name);
      const keyIndex = names.indexOf("__ffdb_primary_key");
      const versionIndex = names.indexOf("__ffdb_row_version");
      const sequenceIndex = names.indexOf("__ffdb_server_sequence");
      if (keyIndex < 0 || versionIndex < 0 || sequenceIndex < 0) continue;
      for (const row of result.rows) {
        const values: Record<string, JsonValue> = {};
        names.forEach((name, index) => {
          const value = row[index];
          if (!name.startsWith("__ffdb_") && value !== undefined) values[name] = value as JsonValue;
        });
        const encodedPrimaryKey = row[keyIndex];
        if (typeof encodedPrimaryKey !== "string") throw new Error("Snapshot primary key is invalid");
        let primaryKey: JsonValue;
        try {
          primaryKey = JSON.parse(encodedPrimaryKey) as JsonValue;
        } catch {
          throw new Error("Snapshot primary key is invalid");
        }
        await this.upsert({
          table,
          primaryKey,
          values,
          rowVersion: Number(row[versionIndex]),
          serverSequence: Number(row[sequenceIndex]),
        });
      }
    }
  }

  async setCursor(cursor: string, schemaVersion: number): Promise<void> {
    await this.driver.execute(
      `INSERT INTO __ffdb_client_meta (key, value) VALUES ('cursor', ?1), ('schema_version', ?2)
       ON CONFLICT (key) DO UPDATE SET value = excluded.value`,
      [cursor, String(schemaVersion)],
    );
  }

  async clearCursor(): Promise<void> {
    await this.driver.execute(
      "DELETE FROM __ffdb_client_meta WHERE key IN ('cursor', 'schema_version')",
    );
  }

  async removePending(mutationIds: readonly string[]): Promise<void> {
    for (const id of mutationIds) {
      await this.driver.execute("DELETE FROM __ffdb_client_pending WHERE mutation_id = ?1", [id]);
    }
  }

  async rejectPending(mutationId: string, errorCode: string): Promise<void> {
    const result = await this.driver.execute<{ readonly payload_json: string }>(
      "SELECT payload_json FROM __ffdb_client_pending WHERE mutation_id = ?1",
      [mutationId],
    );
    const row = result.rows[0];
    if (row === undefined) return;
    await this.driver.execute(
      `INSERT INTO __ffdb_client_rejected (mutation_id, payload_json, error_code, rejected_at_ms)
       VALUES (?1, ?2, ?3, ?4)
       ON CONFLICT (mutation_id) DO UPDATE SET error_code = excluded.error_code, rejected_at_ms = excluded.rejected_at_ms`,
      [mutationId, row.payload_json, errorCode, this.now()],
    );
    await this.removePending([mutationId]);
  }
}

function isAuthTokenPair(value: unknown): value is AuthTokenPair {
  if (!isRecord(value) || !isRecord(value.user)) return false;
  const user = value.user;
  return isNonemptyString(value.access_token)
    && isNonemptyString(value.refresh_token)
    && isNonemptyString(value.token_type)
    && isNonemptyString(value.session_id)
    && isFiniteNumber(value.expires_in_seconds)
    && isNonemptyString(user.id)
    && isNonemptyString(user.email)
    && typeof user.email_verified === "boolean"
    && typeof user.disabled === "boolean"
    && isNonemptyString(user.role)
    && isRecord(user.custom_claims)
    && isFiniteNumber(user.created_at_ms);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isNonemptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function stableJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  return `{${Object.entries(value as Record<string, unknown>)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, entry]) => `${JSON.stringify(key)}:${stableJson(entry)}`)
    .join(",")}}`;
}

function decodeReplicaRow(
  table: string,
  row: {
    readonly primary_key_json: string;
    readonly values_json: string;
    readonly row_version: number;
    readonly server_sequence: number;
  },
): ReplicaRecord {
  return {
    table,
    primaryKey: JSON.parse(row.primary_key_json) as JsonValue,
    values: JSON.parse(row.values_json) as Readonly<Record<string, JsonValue>>,
    rowVersion: row.row_version,
    serverSequence: row.server_sequence,
  };
}

function validateLimit(limit: number): void {
  if (!Number.isSafeInteger(limit) || limit < 1) throw new RangeError("limit must be a positive integer");
}
