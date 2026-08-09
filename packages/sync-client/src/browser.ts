import type { JsonValue, SnapshotResponse } from "@ffdb/client";

import {
  applyOptimisticMutation,
  sameMutationContent,
  type RejectedMutation,
  type PendingMutation,
  type ReplicaAdapter,
  type ReplicaRecord,
  type ReplicaTransaction,
} from "./index.js";

const DATABASE_VERSION = 1;
const META = "meta";
const ROWS = "rows";
const PENDING = "pending";
const REJECTED = "rejected";

interface MetaEntry {
  readonly key: string;
  readonly value: string;
}

interface PendingEntry extends PendingMutation {
  readonly sortKey: readonly [number, string];
}

interface RowEntry extends ReplicaRecord {
  readonly key: string;
}

type RejectedEntry = RejectedMutation;

/** Durable browser replica backed by one transactional IndexedDB database. */
export class IndexedDbReplica implements ReplicaAdapter {
  readonly #database: Promise<IDBDatabase>;
  readonly #now: () => number;

  constructor(name = "ffdb-offline", now: () => number = Date.now) {
    if (name.trim().length === 0) throw new TypeError("IndexedDB database name is required");
    this.#database = openDatabase(name);
    this.#now = now;
  }

  async transaction<T>(work: (transaction: ReplicaTransaction) => Promise<T>): Promise<T> {
    const database = await this.#database;
    const native = database.transaction([META, ROWS, PENDING, REJECTED], "readwrite", {
      durability: "strict",
    });
    const completion = transactionCompletion(native);
    try {
      const result = await work(new IndexedDbReplicaTransaction(native, this.#now));
      await completion;
      return result;
    } catch (error) {
      try {
        native.abort();
      } catch {
        // The transaction may already have committed or aborted.
      }
      await completion.catch(() => undefined);
      throw error;
    }
  }

  async getCursor(): Promise<{ readonly cursor: string; readonly schemaVersion: number } | null> {
    const database = await this.#database;
    const native = database.transaction(META, "readonly");
    const completion = transactionCompletion(native);
    const store = native.objectStore(META);
    const [cursor, version] = await Promise.all([
      request<MetaEntry | undefined>(store.get("cursor")),
      request<MetaEntry | undefined>(store.get("schema_version")),
    ]);
    await completion;
    if (cursor === undefined || version === undefined) return null;
    const schemaVersion = Number(version.value);
    if (!Number.isSafeInteger(schemaVersion) || schemaVersion < 0) {
      throw new Error("Persisted FFDB schema version is invalid");
    }
    return { cursor: cursor.value, schemaVersion };
  }

  async getPending(limit: number): Promise<readonly PendingMutation[]> {
    validateLimit(limit);
    const database = await this.#database;
    const native = database.transaction(PENDING, "readonly");
    const completion = transactionCompletion(native);
    const store = native.objectStore(PENDING);
    const entries = await collectCursor<PendingEntry>(store.index("sort_key").openCursor(), limit);
    await completion;
    return entries.map(({ sortKey: _sortKey, ...mutation }) => mutation);
  }

  async getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null> {
    const database = await this.#database;
    const native = database.transaction(ROWS, "readonly");
    const completion = transactionCompletion(native);
    const entry = await request<RowEntry | undefined>(
      native.objectStore(ROWS).get(rowKey(table, primaryKey)),
    );
    await completion;
    return entry === undefined ? null : decodeRowEntry(entry);
  }

  async listRows(table: string): Promise<readonly ReplicaRecord[]> {
    const database = await this.#database;
    const native = database.transaction(ROWS, "readonly");
    const completion = transactionCompletion(native);
    const prefix = `${table}\u001f`;
    const entries = await collectCursor<RowEntry>(
      native.objectStore(ROWS).openCursor(IDBKeyRange.bound(prefix, `${prefix}\uffff`)),
      Number.MAX_SAFE_INTEGER,
    );
    await completion;
    return entries.map(decodeRowEntry);
  }

  async getRejected(limit: number): Promise<readonly RejectedMutation[]> {
    validateLimit(limit);
    const database = await this.#database;
    const native = database.transaction(REJECTED, "readonly");
    const completion = transactionCompletion(native);
    const entries = await collectCursor<RejectedEntry>(
      native.objectStore(REJECTED).openCursor(),
      Number.MAX_SAFE_INTEGER,
    );
    await completion;
    return [...entries]
      .sort((left, right) =>
        left.rejectedAtMs - right.rejectedAtMs || left.mutation_id.localeCompare(right.mutation_id))
      .slice(0, limit);
  }

  async enqueue(mutation: PendingMutation): Promise<void> {
    const database = await this.#database;
    const native = database.transaction([ROWS, PENDING, REJECTED], "readwrite", { durability: "strict" });
    const completion = transactionCompletion(native);
    try {
      const store = native.objectStore(PENDING);
      const existing = await request<PendingEntry | undefined>(store.get(mutation.mutation_id));
      if (existing !== undefined) {
        const { sortKey: _sortKey, ...persisted } = existing;
        if (!sameMutationContent(persisted, mutation)) {
          throw new Error(`Mutation id ${mutation.mutation_id} was reused`);
        }
      } else {
        const rejected = await request<RejectedEntry | undefined>(
          native.objectStore(REJECTED).get(mutation.mutation_id),
        );
        if (rejected !== undefined) {
          if (!sameMutationContent(rejected, mutation)) {
            throw new Error(`Mutation id ${mutation.mutation_id} was reused`);
          }
        } else {
          await request(store.add({ ...mutation, sortKey: [mutation.enqueuedAtMs, mutation.mutation_id] }));
          await applyOptimisticMutation(new IndexedDbReplicaTransaction(native, this.#now), mutation);
        }
      }
      await completion;
    } catch (error) {
      try {
        native.abort();
      } catch {
        // The transaction may already have committed or aborted.
      }
      await completion.catch(() => undefined);
      throw error;
    }
  }

  async close(): Promise<void> {
    (await this.#database).close();
  }
}

class IndexedDbReplicaTransaction implements ReplicaTransaction {
  constructor(
    private readonly transaction: IDBTransaction,
    private readonly now: () => number,
  ) {}

  async getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null> {
    const entry = await request<RowEntry | undefined>(
      this.transaction.objectStore(ROWS).get(rowKey(table, primaryKey)),
    );
    return entry === undefined ? null : decodeRowEntry(entry);
  }

  async getPending(limit: number): Promise<readonly PendingMutation[]> {
    validateLimit(limit);
    const entries = await collectCursor<PendingEntry>(
      this.transaction.objectStore(PENDING).index("sort_key").openCursor(),
      limit,
    );
    return entries.map(({ sortKey: _sortKey, ...mutation }) => mutation);
  }

  async upsert(record: ReplicaRecord): Promise<void> {
    const store = this.transaction.objectStore(ROWS);
    const key = rowKey(record.table, record.primaryKey);
    const existing = await request<ReplicaRecord | undefined>(store.get(key));
    if (existing === undefined || existing.serverSequence <= record.serverSequence) {
      await request(store.put({ ...record, key }));
    }
  }

  async delete(table: string, primaryKey: JsonValue, _rowVersion: number, serverSequence: number): Promise<void> {
    const store = this.transaction.objectStore(ROWS);
    const key = rowKey(table, primaryKey);
    const existing = await request<ReplicaRecord | undefined>(store.get(key));
    if (existing === undefined || existing.serverSequence <= serverSequence) await request(store.delete(key));
  }

  async replaceSnapshot(snapshot: SnapshotResponse): Promise<void> {
    const store = this.transaction.objectStore(ROWS);
    await request(store.clear());
    for (const [table, result] of Object.entries(snapshot.tables)) {
      for (const record of decodeSnapshotTable(table, result)) {
        await request(store.put({ ...record, key: rowKey(record.table, record.primaryKey) }));
      }
    }
  }

  async setCursor(cursor: string, schemaVersion: number): Promise<void> {
    const store = this.transaction.objectStore(META);
    await request(store.put({ key: "cursor", value: cursor } satisfies MetaEntry));
    await request(store.put({ key: "schema_version", value: String(schemaVersion) } satisfies MetaEntry));
  }

  async clearCursor(): Promise<void> {
    const store = this.transaction.objectStore(META);
    await request(store.delete("cursor"));
    await request(store.delete("schema_version"));
  }

  async removePending(mutationIds: readonly string[]): Promise<void> {
    const store = this.transaction.objectStore(PENDING);
    for (const id of mutationIds) await request(store.delete(id));
  }

  async rejectPending(mutationId: string, errorCode: string): Promise<void> {
    const pending = this.transaction.objectStore(PENDING);
    const mutation = await request<PendingEntry | undefined>(pending.get(mutationId));
    if (mutation === undefined) return;
    const { sortKey: _sortKey, ...payload } = mutation;
    await request(this.transaction.objectStore(REJECTED).put({
      ...payload,
      errorCode,
      rejectedAtMs: this.now(),
    } satisfies RejectedEntry));
    await request(pending.delete(mutationId));
  }
}

function openDatabase(name: string): Promise<IDBDatabase> {
  if (typeof indexedDB === "undefined") {
    return Promise.reject(new Error("IndexedDB is unavailable in this runtime"));
  }
  return new Promise((resolve, reject) => {
    const open = indexedDB.open(name, DATABASE_VERSION);
    open.onupgradeneeded = () => {
      const database = open.result;
      if (!database.objectStoreNames.contains(META)) database.createObjectStore(META, { keyPath: "key" });
      if (!database.objectStoreNames.contains(ROWS)) database.createObjectStore(ROWS, { keyPath: "key" });
      if (!database.objectStoreNames.contains(PENDING)) {
        const store = database.createObjectStore(PENDING, { keyPath: "mutation_id" });
        store.createIndex("sort_key", "sortKey", { unique: false });
      }
      if (!database.objectStoreNames.contains(REJECTED)) {
        database.createObjectStore(REJECTED, { keyPath: "mutation_id" });
      }
    };
    open.onblocked = () => reject(new Error("IndexedDB upgrade is blocked by another tab"));
    open.onerror = () => reject(open.error ?? new Error("Unable to open IndexedDB"));
    open.onsuccess = () => resolve(open.result);
  });
}

function request<T = IDBValidKey>(operation: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    operation.onsuccess = () => resolve(operation.result);
    operation.onerror = () => reject(operation.error ?? new Error("IndexedDB operation failed"));
  });
}

function transactionCompletion(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = () => reject(transaction.error ?? new Error("IndexedDB transaction aborted"));
    transaction.onerror = () => reject(transaction.error ?? new Error("IndexedDB transaction failed"));
  });
}

function collectCursor<T>(operation: IDBRequest<IDBCursorWithValue | null>, limit: number): Promise<readonly T[]> {
  return new Promise((resolve, reject) => {
    const results: T[] = [];
    operation.onerror = () => reject(operation.error ?? new Error("IndexedDB cursor failed"));
    operation.onsuccess = () => {
      const cursor = operation.result;
      if (cursor === null || results.length >= limit) {
        resolve(results);
        return;
      }
      results.push(cursor.value as T);
      cursor.continue();
    };
  });
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

function rowKey(table: string, primaryKey: JsonValue): string {
  return `${table}\u001f${stableJson(primaryKey)}`;
}

function decodeRowEntry({ key: _key, ...record }: RowEntry): ReplicaRecord {
  return record;
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
