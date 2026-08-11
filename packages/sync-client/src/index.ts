import type {
  FFDBClient,
  JsonValue,
  LogicalChange,
  QueryResult,
  SnapshotResponse,
  SyncMutation,
  SyncMutationResult,
} from "@ffdb/client";

export interface ReplicaRecord {
  readonly table: string;
  readonly primaryKey: JsonValue;
  readonly values: Readonly<Record<string, JsonValue>>;
  readonly rowVersion: number;
  readonly serverSequence: number;
}

export interface PendingMutation extends SyncMutation {
  readonly enqueuedAtMs: number;
  readonly attempts: number;
}

export interface RejectedMutation extends PendingMutation {
  readonly errorCode: string;
  readonly rejectedAtMs: number;
}

export interface ReplicaTransaction {
  getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null>;
  getPending(limit: number): Promise<readonly PendingMutation[]>;
  upsert(record: ReplicaRecord): Promise<void>;
  delete(table: string, primaryKey: JsonValue, rowVersion: number, serverSequence: number): Promise<void>;
  replaceSnapshot(snapshot: SnapshotResponse): Promise<void>;
  setCursor(cursor: string, schemaVersion: number): Promise<void>;
  clearCursor(): Promise<void>;
  removePending(mutationIds: readonly string[]): Promise<void>;
  rejectPending(mutationId: string, errorCode: string): Promise<void>;
}

export interface ReplicaAdapter {
  transaction<T>(work: (transaction: ReplicaTransaction) => Promise<T>): Promise<T>;
  getCursor(): Promise<{ readonly cursor: string; readonly schemaVersion: number } | null>;
  getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null>;
  listRows(table: string): Promise<readonly ReplicaRecord[]>;
  getPending(limit: number): Promise<readonly PendingMutation[]>;
  getRejected(limit: number): Promise<readonly RejectedMutation[]>;
  enqueue(mutation: PendingMutation): Promise<void>;
}

export type SyncPhase = "idle" | "snapshot" | "push" | "pull" | "error";

export type AutoSyncStatus = "stopped" | "paused" | "syncing" | "watching" | "backoff";

export interface SyncState {
  readonly phase: SyncPhase;
  readonly autoSync: AutoSyncStatus;
  readonly lastSyncedAtMs: number | null;
  readonly lastChangedAtMs: number | null;
  readonly pending: number;
  readonly error: Error | null;
}

export interface OfflineSyncOptions {
  readonly pushBatchSize?: number;
  readonly pullBatchSize?: number;
  readonly now?: () => number;
}

export interface AutoSyncOptions {
  /** Run a full push/pull cycle as soon as automatic sync starts. Defaults to true. */
  readonly syncOnStart?: boolean;
  /** Debounce and push optimistic mutations automatically. Defaults to true. */
  readonly syncOnMutation?: boolean;
  readonly mutationDebounceMs?: number;
  /** Fallback cadence for servers that do not support waiting pulls. */
  readonly pollIntervalMs?: number;
  /** Duration of each authenticated waiting pull. Set to 0 for polling only. */
  readonly longPollMs?: number;
  readonly retryMinMs?: number;
  readonly retryMaxMs?: number;
  readonly active?: boolean;
  readonly online?: boolean;
  /** Primarily useful for deterministic retry-jitter tests. */
  readonly random?: () => number;
}

export type AutoSyncWakeReason = "focus" | "mutation" | "manual";

export interface AutoSyncController {
  readonly status: AutoSyncStatus;
  setActive(active: boolean): void;
  setOnline(online: boolean): void;
  wake(reason?: AutoSyncWakeReason): void;
  stop(): void;
}

export interface SyncWaitResult {
  readonly changed: boolean;
  readonly waitedMs: number;
}

interface WaitingPull {
  readonly controller: AbortController;
  promise: Promise<SyncWaitResult>;
}

export class OfflineSyncClient {
  readonly #client: FFDBClient;
  readonly #replica: ReplicaAdapter;
  readonly #pushBatchSize: number;
  readonly #pullBatchSize: number;
  readonly #now: () => number;
  readonly #listeners = new Set<(state: SyncState) => void>();
  #state: SyncState = {
    phase: "idle",
    autoSync: "stopped",
    lastSyncedAtMs: null,
    lastChangedAtMs: null,
    pending: 0,
    error: null,
  };
  #running: Promise<void> | null = null;
  #waiting: WaitingPull | null = null;
  #autoController: AutoSyncControllerImpl | null = null;

  constructor(client: FFDBClient, replica: ReplicaAdapter, options: OfflineSyncOptions = {}) {
    this.#client = client;
    this.#replica = replica;
    this.#pushBatchSize = bounded(options.pushBatchSize ?? 100, 1, 100);
    this.#pullBatchSize = bounded(options.pullBatchSize ?? 1_000, 1, 1_000);
    this.#now = options.now ?? Date.now;
  }

  get state(): SyncState {
    return this.#state;
  }

  subscribe(listener: (state: SyncState) => void): () => void {
    this.#listeners.add(listener);
    listener(this.#state);
    return () => this.#listeners.delete(listener);
  }

  getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null> {
    validateTable(table);
    return this.#replica.getRow(table, primaryKey);
  }

  listRows(table: string): Promise<readonly ReplicaRecord[]> {
    validateTable(table);
    return this.#replica.listRows(table);
  }

  getPending(limit = 100): Promise<readonly PendingMutation[]> {
    validateLimit(limit);
    return this.#replica.getPending(limit);
  }

  getRejected(limit = 100): Promise<readonly RejectedMutation[]> {
    validateLimit(limit);
    return this.#replica.getRejected(limit);
  }

  async mutate(mutation: SyncMutation): Promise<void> {
    validateMutation(mutation);
    await this.#replica.enqueue({ ...mutation, enqueuedAtMs: this.#now(), attempts: 0 });
    const pending = (await this.#replica.getPending(Number.MAX_SAFE_INTEGER)).length;
    this.#publish({ ...this.#state, pending });
    this.#autoController?.wake("mutation");
  }

  sync(signal?: AbortSignal): Promise<void> {
    if (this.#running !== null) return this.#running;
    this.#running = this.#cancelWaitingPull()
      .then(() => this.#performSync(signal))
      .catch((cause: unknown) => {
        const error = cause instanceof Error ? cause : new Error("Sync failed");
        if (isAbortError(error)) {
          this.#publish({ ...this.#state, phase: "idle", error: null });
        } else {
          this.#publish({ ...this.#state, phase: "error", error });
        }
        throw error;
      })
      .finally(() => {
        this.#running = null;
      });
    return this.#running;
  }

  waitForChanges(waitMs = 25_000, signal?: AbortSignal): Promise<SyncWaitResult> {
    boundedDuration(waitMs, 0, 30_000, "waitMs");
    if (this.#waiting !== null) return this.#waiting.promise;

    const controller = new AbortController();
    const waiting: WaitingPull = {
      controller,
      promise: Promise.resolve({ changed: false, waitedMs: 0 }),
    };
    const unlink = forwardAbort(signal, controller);
    const startedAt = Date.now();
    waiting.promise = (async () => {
      if (this.#running !== null) await this.#running;
      const changed = await this.#performWaitingPull(waitMs, controller.signal);
      return { changed, waitedMs: Math.max(0, Date.now() - startedAt) };
    })()
      .catch((cause: unknown) => {
        const error = cause instanceof Error ? cause : new Error("Waiting sync failed");
        if (!isAbortError(error)) this.#publish({ ...this.#state, phase: "error", error });
        throw error;
      })
      .finally(() => {
        unlink();
        if (this.#waiting === waiting) this.#waiting = null;
      });
    this.#waiting = waiting;
    return waiting.promise;
  }

  startAutoSync(options: AutoSyncOptions = {}): AutoSyncController {
    this.#autoController?.stop();
    const controller = new AutoSyncControllerImpl(
      (signal) => this.sync(signal),
      (waitMs, signal) => this.waitForChanges(waitMs, signal),
      (status) => this.#setAutoSyncStatus(status),
      options,
      () => {
        if (this.#autoController === controller) this.#autoController = null;
      },
    );
    this.#autoController = controller;
    controller.start();
    return controller;
  }

  async #performSync(signal?: AbortSignal): Promise<void> {
    let position = await this.#replica.getCursor();
    let changed = false;
    if (position === null) {
      this.#publish({ ...this.#state, phase: "snapshot", error: null });
      position = await this.#resnapshot(signal);
      changed = true;
    }

    this.#publish({ ...this.#state, phase: "push", error: null });
    let pending = await this.#replica.getPending(this.#pushBatchSize);
    let requiresAuthoritativeSnapshot = false;
    while (pending.length > 0) {
      const result = await this.#client.sync.push(
        {
          schema_version: position.schemaVersion,
          mutations: pending.map(stripPendingMetadata),
        },
        { ...(signal === undefined ? {} : { signal }), idempotencyKey: batchIdempotencyKey(pending) },
      );
      await this.#replica.transaction(async (transaction) => {
        requiresAuthoritativeSnapshot = await consumePushResults(
          transaction,
          pending,
          result.results,
        ) || requiresAuthoritativeSnapshot;
      });
      pending = await this.#replica.getPending(this.#pushBatchSize);
    }

    // A duplicate, superseded, or rejected mutation may not emit a new logical
    // change after our current cursor. Replace optimistic state with a fresh
    // authoritative snapshot before continuing the pull in those cases.
    if (requiresAuthoritativeSnapshot) {
      position = await this.#resnapshot(signal);
      changed = true;
    }

    this.#publish({ ...this.#state, phase: "pull", pending: 0, error: null });
    const pulled = await this.#pullFrom(position, signal);
    changed = pulled.changed || changed;
    const syncedAt = this.#now();
    this.#publish({
      ...this.#state,
      phase: "idle",
      lastSyncedAtMs: syncedAt,
      lastChangedAtMs: changed
        ? nextChangedAt(syncedAt, this.#state.lastChangedAtMs)
        : this.#state.lastChangedAtMs,
      pending: 0,
      error: null,
    });
  }

  async #performWaitingPull(waitMs: number, signal: AbortSignal): Promise<boolean> {
    const position = await this.#replica.getCursor();
    if (position === null) {
      await this.#performSync(signal);
      return true;
    }
    const pulled = await this.#pullFrom(position, signal, waitMs);
    const syncedAt = this.#now();
    this.#publish({
      ...this.#state,
      phase: "idle",
      lastSyncedAtMs: syncedAt,
      lastChangedAtMs: pulled.changed
        ? nextChangedAt(syncedAt, this.#state.lastChangedAtMs)
        : this.#state.lastChangedAtMs,
      pending: (await this.#replica.getPending(Number.MAX_SAFE_INTEGER)).length,
      error: null,
    });
    return pulled.changed;
  }

  async #pullFrom(
    initialPosition: { readonly cursor: string; readonly schemaVersion: number },
    signal?: AbortSignal,
    waitMs = 0,
  ): Promise<{ readonly position: { readonly cursor: string; readonly schemaVersion: number }; readonly changed: boolean }> {
    let position = initialPosition;
    let changed = false;
    let first = true;
    let hasMore = true;
    while (hasMore) {
      const result = await this.#client.sync.pull(position.cursor, this.#pullBatchSize, {
        ...(signal === undefined ? {} : { signal }),
        ...(first && waitMs > 0 ? { waitMs } : {}),
      });
      first = false;
      if (result.control?.type === "resnapshot_required" || result.control?.type === "invalidate_scope") {
        position = await this.#resnapshot(signal);
        changed = true;
        break;
      }
      const schemaVersion = position.schemaVersion;
      await this.#replica.transaction(async (transaction) => {
        for (const change of result.changes) await applyChange(transaction, change);
        await transaction.setCursor(result.cursor, schemaVersion);
      });
      position = { ...position, cursor: result.cursor };
      changed = result.changes.length > 0 || changed;
      hasMore = result.has_more;
    }
    return { position, changed };
  }

  async #resnapshot(signal?: AbortSignal): Promise<{ readonly cursor: string; readonly schemaVersion: number }> {
    const snapshot = await this.#client.sync.snapshot(
      undefined,
      signal === undefined ? {} : { signal },
    );
    await this.#replica.transaction(async (transaction) => {
      await transaction.replaceSnapshot(snapshot);
      const pending = await transaction.getPending(Number.MAX_SAFE_INTEGER);
      for (const mutation of pending) await applyOptimisticMutation(transaction, mutation);
      await transaction.setCursor(snapshot.cursor, snapshot.schema_version);
    });
    return { cursor: snapshot.cursor, schemaVersion: snapshot.schema_version };
  }

  async #cancelWaitingPull(): Promise<void> {
    const waiting = this.#waiting;
    if (waiting === null) return;
    waiting.controller.abort();
    try {
      await waiting.promise;
    } catch (error) {
      if (!isAbortError(error)) throw error;
    }
  }

  #setAutoSyncStatus(autoSync: AutoSyncStatus): void {
    if (this.#state.autoSync !== autoSync) this.#publish({ ...this.#state, autoSync });
  }

  #publish(state: SyncState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener(state);
  }
}

interface NormalizedAutoSyncOptions {
  readonly syncOnStart: boolean;
  readonly syncOnMutation: boolean;
  readonly mutationDebounceMs: number;
  readonly pollIntervalMs: number;
  readonly longPollMs: number;
  readonly retryMinMs: number;
  readonly retryMaxMs: number;
  readonly active: boolean;
  readonly online: boolean;
  readonly random: () => number;
}

class AutoSyncControllerImpl implements AutoSyncController {
  readonly #sync: (signal: AbortSignal) => Promise<void>;
  readonly #wait: (waitMs: number, signal: AbortSignal) => Promise<SyncWaitResult>;
  readonly #publishStatus: (status: AutoSyncStatus) => void;
  readonly #options: NormalizedAutoSyncOptions;
  readonly #onStop: () => void;
  #active: boolean;
  #online: boolean;
  #stopped = false;
  #running = false;
  #rerun = false;
  #rerunDelayMs = 0;
  #retryAttempt = 0;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #request: AbortController | null = null;
  #status: AutoSyncStatus = "stopped";

  constructor(
    sync: (signal: AbortSignal) => Promise<void>,
    wait: (waitMs: number, signal: AbortSignal) => Promise<SyncWaitResult>,
    publishStatus: (status: AutoSyncStatus) => void,
    options: AutoSyncOptions,
    onStop: () => void,
  ) {
    this.#sync = sync;
    this.#wait = wait;
    this.#publishStatus = publishStatus;
    this.#options = normalizeAutoSyncOptions(options);
    this.#onStop = onStop;
    this.#active = this.#options.active;
    this.#online = this.#options.online;
  }

  get status(): AutoSyncStatus {
    return this.#status;
  }

  start(): void {
    if (this.#stopped) return;
    if (!this.#active || !this.#online) {
      this.#setStatus("paused");
      return;
    }
    this.#schedule(this.#options.syncOnStart ? "sync" : "watch", 0);
  }

  setActive(active: boolean): void {
    if (this.#stopped || this.#active === active) return;
    this.#active = active;
    this.#reconcileAvailability();
  }

  setOnline(online: boolean): void {
    if (this.#stopped || this.#online === online) return;
    this.#online = online;
    this.#reconcileAvailability();
  }

  wake(reason: AutoSyncWakeReason = "manual"): void {
    if (this.#stopped || (reason === "mutation" && !this.#options.syncOnMutation)) return;
    if (!this.#active || !this.#online) {
      this.#rerun = true;
      this.#setStatus("paused");
      return;
    }
    this.#rerun = true;
    this.#rerunDelayMs = reason === "mutation" ? this.#options.mutationDebounceMs : 0;
    this.#request?.abort();
    if (!this.#running) {
      this.#scheduleRerun();
    }
  }

  stop(): void {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#clearTimer();
    this.#request?.abort();
    this.#setStatus("stopped");
    this.#onStop();
  }

  #reconcileAvailability(): void {
    if (!this.#active || !this.#online) {
      this.#clearTimer();
      this.#request?.abort();
      this.#setStatus("paused");
      return;
    }
    this.#retryAttempt = 0;
    this.#rerun = true;
    this.#rerunDelayMs = 0;
    this.#request?.abort();
    if (!this.#running) this.#schedule("sync", 0);
  }

  #schedule(kind: "sync" | "watch", delayMs: number): void {
    if (this.#stopped || !this.#active || !this.#online) return;
    this.#clearTimer();
    this.#timer = setTimeout(() => {
      this.#timer = null;
      void this.#run(kind);
    }, delayMs);
    unrefTimer(this.#timer);
  }

  async #run(kind: "sync" | "watch"): Promise<void> {
    if (this.#stopped || !this.#active || !this.#online || this.#running) return;
    this.#running = true;
    this.#rerun = false;
    const request = new AbortController();
    this.#request = request;
    this.#setStatus(kind === "sync" ? "syncing" : "watching");
    try {
      const result = kind === "sync"
        ? (await this.#sync(request.signal), null)
        : await this.#wait(this.#options.longPollMs, request.signal);
      this.#retryAttempt = 0;
      if (this.#stopped || !this.#active || !this.#online) return;
      if (this.#rerun) {
        this.#scheduleRerun();
      } else if (kind === "watch" && result !== null && waitReturnedTooQuickly(result, this.#options.longPollMs)) {
        this.#schedule("sync", this.#options.pollIntervalMs);
      } else if (this.#options.longPollMs > 0) {
        this.#schedule("watch", 0);
      } else {
        this.#schedule("sync", this.#options.pollIntervalMs);
      }
    } catch (error) {
      if (this.#stopped || !this.#active || !this.#online) return;
      if (isAbortError(error)) {
        if (this.#rerun) this.#scheduleRerun();
        else this.#schedule("watch", 0);
      } else {
        this.#retryAttempt += 1;
        this.#setStatus("backoff");
        this.#schedule("sync", retryDelay(this.#retryAttempt, this.#options));
      }
    } finally {
      if (this.#request === request) this.#request = null;
      this.#running = false;
    }
  }

  #scheduleRerun(): void {
    const delayMs = this.#rerunDelayMs;
    this.#rerunDelayMs = 0;
    this.#schedule("sync", delayMs);
  }

  #clearTimer(): void {
    if (this.#timer === null) return;
    clearTimeout(this.#timer);
    this.#timer = null;
  }

  #setStatus(status: AutoSyncStatus): void {
    this.#status = status;
    this.#publishStatus(status);
  }
}

export class MemoryReplica implements ReplicaAdapter, ReplicaTransaction {
  readonly #rows = new Map<string, ReplicaRecord>();
  readonly #pending = new Map<string, PendingMutation>();
  readonly #rejected = new Map<string, RejectedMutation>();
  readonly #now: () => number;
  #cursor: { readonly cursor: string; readonly schemaVersion: number } | null = null;

  constructor(now: () => number = Date.now) {
    this.#now = now;
  }

  async transaction<T>(work: (transaction: ReplicaTransaction) => Promise<T>): Promise<T> {
    const rows = new Map(this.#rows);
    const pending = new Map(this.#pending);
    const rejected = new Map(this.#rejected);
    const cursor = this.#cursor;
    try {
      return await work(this);
    } catch (error) {
      this.#rows.clear();
      for (const [key, value] of rows) this.#rows.set(key, value);
      this.#pending.clear();
      for (const [key, value] of pending) this.#pending.set(key, value);
      this.#rejected.clear();
      for (const [key, value] of rejected) this.#rejected.set(key, value);
      this.#cursor = cursor;
      throw error;
    }
  }

  async getCursor(): Promise<{ readonly cursor: string; readonly schemaVersion: number } | null> {
    return this.#cursor;
  }

  async getPending(limit: number): Promise<readonly PendingMutation[]> {
    validateLimit(limit);
    return sortPending(this.#pending.values()).slice(0, limit);
  }

  async getRow(table: string, primaryKey: JsonValue): Promise<ReplicaRecord | null> {
    return this.#rows.get(replicaKey(table, primaryKey)) ?? null;
  }

  async listRows(table: string): Promise<readonly ReplicaRecord[]> {
    return [...this.#rows.values()]
      .filter((record) => record.table === table)
      .sort(compareReplicaRecords);
  }

  async getRejected(limit: number): Promise<readonly RejectedMutation[]> {
    validateLimit(limit);
    return sortRejected(this.#rejected.values()).slice(0, limit);
  }

  async enqueue(mutation: PendingMutation): Promise<void> {
    await this.transaction(async (transaction) => {
      const existing = this.#pending.get(mutation.mutation_id);
      if (existing !== undefined) {
        if (!sameMutationContent(existing, mutation)) {
          throw new Error(`Mutation id ${mutation.mutation_id} was reused`);
        }
        return;
      }
      const rejected = this.#rejected.get(mutation.mutation_id);
      if (rejected !== undefined) {
        if (!sameMutationContent(rejected, mutation)) {
          throw new Error(`Mutation id ${mutation.mutation_id} was reused`);
        }
        return;
      }
      this.#pending.set(mutation.mutation_id, mutation);
      await applyOptimisticMutation(transaction, mutation);
    });
  }

  async upsert(record: ReplicaRecord): Promise<void> {
    const key = replicaKey(record.table, record.primaryKey);
    const existing = this.#rows.get(key);
    if (existing === undefined || existing.serverSequence <= record.serverSequence) this.#rows.set(key, record);
  }

  async delete(
    table: string,
    primaryKey: JsonValue,
    _rowVersion: number,
    serverSequence: number,
  ): Promise<void> {
    const key = replicaKey(table, primaryKey);
    const existing = this.#rows.get(key);
    if (existing === undefined || existing.serverSequence <= serverSequence) this.#rows.delete(key);
  }

  async replaceSnapshot(snapshot: SnapshotResponse): Promise<void> {
    this.#rows.clear();
    for (const [table, result] of Object.entries(snapshot.tables)) {
      const decoded = decodeSnapshotTable(table, result);
      for (const row of decoded) this.#rows.set(replicaKey(row.table, row.primaryKey), row);
    }
  }

  async setCursor(cursor: string, schemaVersion: number): Promise<void> {
    this.#cursor = { cursor, schemaVersion };
  }

  async clearCursor(): Promise<void> {
    this.#cursor = null;
  }

  async removePending(mutationIds: readonly string[]): Promise<void> {
    for (const id of mutationIds) this.#pending.delete(id);
  }

  async rejectPending(mutationId: string, errorCode: string): Promise<void> {
    const mutation = this.#pending.get(mutationId);
    if (mutation === undefined) return;
    this.#pending.delete(mutationId);
    this.#rejected.set(mutationId, { ...mutation, errorCode, rejectedAtMs: this.#now() });
  }

  rows(): readonly ReplicaRecord[] {
    return [...this.#rows.values()].sort((left, right) => {
      const byTable = left.table.localeCompare(right.table);
      return byTable === 0 ? compareReplicaRecords(left, right) : byTable;
    });
  }

  rejected(): readonly RejectedMutation[] {
    return sortRejected(this.#rejected.values());
  }
}

async function consumePushResults(
  transaction: ReplicaTransaction,
  pending: readonly PendingMutation[],
  results: readonly SyncMutationResult[],
): Promise<boolean> {
  const pendingIds = new Set(pending.map((mutation) => mutation.mutation_id));
  if (results.length !== pendingIds.size) {
    throw new Error("Server did not return exactly one result for every mutation");
  }
  const returnedIds = new Set<string>();
  let requiresAuthoritativeSnapshot = false;
  for (const result of results) {
    if (!pendingIds.has(result.mutation_id)) throw new Error("Server returned an unknown mutation id");
    if (returnedIds.has(result.mutation_id)) throw new Error("Server returned a duplicate mutation result");
    returnedIds.add(result.mutation_id);
    if (result.status === "applied" || result.status === "duplicate" || result.status === "superseded") {
      await transaction.removePending([result.mutation_id]);
      if (result.status !== "applied") requiresAuthoritativeSnapshot = true;
    } else {
      await transaction.rejectPending(result.mutation_id, result.error_code ?? "sync.rejected");
      requiresAuthoritativeSnapshot = true;
    }
  }
  if (requiresAuthoritativeSnapshot) await transaction.clearCursor();
  return requiresAuthoritativeSnapshot;
}

/** Apply a queued mutation to the visible replica while retaining server metadata. */
export async function applyOptimisticMutation(
  transaction: ReplicaTransaction,
  mutation: PendingMutation,
): Promise<void> {
  const existing = await transaction.getRow(mutation.table, mutation.primary_key);
  if (mutation.operation === "delete") {
    if (existing !== null) {
      await transaction.delete(
        mutation.table,
        mutation.primary_key,
        existing.rowVersion,
        existing.serverSequence,
      );
    }
    return;
  }
  if (mutation.values === null) {
    throw new TypeError(`${mutation.operation} mutation values are required`);
  }
  await transaction.upsert({
    table: mutation.table,
    primaryKey: mutation.primary_key,
    values: mutation.operation === "update" && existing !== null
      ? { ...existing.values, ...mutation.values }
      : mutation.values,
    rowVersion: existing?.rowVersion ?? mutation.base_row_version ?? 0,
    serverSequence: existing?.serverSequence ?? -1,
  });
}

/** Compare only the server-visible mutation content, excluding local queue metadata. */
export function sameMutationContent(left: SyncMutation, right: SyncMutation): boolean {
  return stableJson(stripPendingMetadata(left)) === stableJson(stripPendingMetadata(right));
}

async function applyChange(transaction: ReplicaTransaction, change: LogicalChange): Promise<void> {
  const primaryKey = normalizePrimaryKey(change.primary_key);
  const legacyPrimaryKey = stableJson(primaryKey) === stableJson(change.primary_key)
    ? null
    : change.primary_key;
  if (legacyPrimaryKey !== null) {
    await transaction.delete(change.table, legacyPrimaryKey, change.row_version, change.sequence);
  }
  if (change.operation === "delete") {
    await transaction.delete(change.table, primaryKey, change.row_version, change.sequence);
  } else {
    if (change.values === null) throw new Error("Upsert change is missing values");
    await transaction.upsert({
      table: change.table,
      primaryKey,
      values: change.values,
      rowVersion: change.row_version,
      serverSequence: change.sequence,
    });
  }
}

function normalizePrimaryKey(primaryKey: JsonValue): JsonValue {
  if (primaryKey === null || Array.isArray(primaryKey) || typeof primaryKey !== "object") {
    return primaryKey;
  }
  const entries = Object.entries(primaryKey);
  return entries.length === 1 ? entries[0]![1] : primaryKey;
}

function decodeSnapshotTable(table: string, result: QueryResult): readonly ReplicaRecord[] {
  const names = result.columns.map((column) => column.name);
  const primaryKeyIndex = names.indexOf("__ffdb_primary_key");
  const rowVersionIndex = names.indexOf("__ffdb_row_version");
  const sequenceIndex = names.indexOf("__ffdb_server_sequence");
  if (primaryKeyIndex < 0 || rowVersionIndex < 0 || sequenceIndex < 0) return [];
  return result.rows.map((cells) => {
    const values: Record<string, JsonValue> = {};
    for (let index = 0; index < names.length; index += 1) {
      const name = names[index];
      const cell = cells[index];
      if (name === undefined || cell === undefined || name.startsWith("__ffdb_")) continue;
      values[name] = cell as JsonValue;
    }
    const encodedPrimaryKey = cells[primaryKeyIndex];
    if (typeof encodedPrimaryKey !== "string") throw new Error("Snapshot primary key is invalid");
    let primaryKey: JsonValue;
    try {
      primaryKey = JSON.parse(encodedPrimaryKey) as JsonValue;
    } catch {
      throw new Error("Snapshot primary key is invalid");
    }
    return {
      table,
      primaryKey,
      values,
      rowVersion: Number(cells[rowVersionIndex]),
      serverSequence: Number(cells[sequenceIndex]),
    };
  });
}

function stripPendingMetadata(mutation: SyncMutation): SyncMutation {
  return {
    mutation_id: mutation.mutation_id,
    table: mutation.table,
    primary_key: mutation.primary_key,
    operation: mutation.operation,
    values: mutation.values,
    base_row_version: mutation.base_row_version,
    client_timestamp_ms: mutation.client_timestamp_ms,
  };
}

function batchIdempotencyKey(mutations: readonly PendingMutation[]): string {
  return `sync-${mutations.map((mutation) => mutation.mutation_id).join("-")}`.slice(0, 240);
}

function validateMutation(mutation: SyncMutation): void {
  if (!mutation.mutation_id || !/^[A-Za-z0-9._:-]{1,200}$/.test(mutation.mutation_id)) {
    throw new TypeError("mutation_id is invalid");
  }
  validateTable(mutation.table);
  if (mutation.operation !== "delete" && mutation.values === null) {
    throw new TypeError(`${mutation.operation} mutation values are required`);
  }
}

function validateTable(table: string): void {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(table) || table.startsWith("__ffdb_")) {
    throw new TypeError("table is invalid");
  }
}

function replicaKey(table: string, primaryKey: JsonValue): string {
  return `${table}\u001f${stableJson(primaryKey)}`;
}

function stableJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  return `{${Object.entries(value as Record<string, unknown>)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, entry]) => `${JSON.stringify(key)}:${stableJson(entry)}`)
    .join(",")}}`;
}

function bounded(value: number, minimum: number, maximum: number): number {
  if (!Number.isInteger(value) || value < minimum || value > maximum) {
    throw new RangeError(`value must be between ${minimum} and ${maximum}`);
  }
  return value;
}

function boundedDuration(value: number, minimum: number, maximum: number, name: string): number {
  if (!Number.isFinite(value) || value < minimum || value > maximum) {
    throw new RangeError(`${name} must be between ${minimum} and ${maximum}`);
  }
  return value;
}

function normalizeAutoSyncOptions(options: AutoSyncOptions): NormalizedAutoSyncOptions {
  const retryMinMs = boundedDuration(options.retryMinMs ?? 1_000, 1, 300_000, "retryMinMs");
  const retryMaxMs = boundedDuration(options.retryMaxMs ?? 30_000, retryMinMs, 3_600_000, "retryMaxMs");
  return {
    syncOnStart: options.syncOnStart ?? true,
    syncOnMutation: options.syncOnMutation ?? true,
    mutationDebounceMs: boundedDuration(options.mutationDebounceMs ?? 250, 0, 60_000, "mutationDebounceMs"),
    pollIntervalMs: boundedDuration(options.pollIntervalMs ?? 15_000, 250, 3_600_000, "pollIntervalMs"),
    longPollMs: boundedDuration(options.longPollMs ?? 25_000, 0, 30_000, "longPollMs"),
    retryMinMs,
    retryMaxMs,
    active: options.active ?? true,
    online: options.online ?? true,
    random: options.random ?? Math.random,
  };
}

function waitReturnedTooQuickly(result: SyncWaitResult, requestedMs: number): boolean {
  if (result.changed || requestedMs === 0) return false;
  return result.waitedMs < Math.min(1_000, Math.max(100, requestedMs / 2));
}

function nextChangedAt(now: number, previous: number | null): number {
  return previous === null ? now : Math.max(now, previous + 1);
}

function retryDelay(attempt: number, options: NormalizedAutoSyncOptions): number {
  const ceiling = Math.min(options.retryMaxMs, options.retryMinMs * 2 ** Math.max(0, attempt - 1));
  return Math.round(ceiling * (0.5 + options.random() * 0.5));
}

function forwardAbort(source: AbortSignal | undefined, destination: AbortController): () => void {
  if (source === undefined) return () => undefined;
  const abort = () => destination.abort(source.reason);
  if (source.aborted) abort();
  else source.addEventListener("abort", abort, { once: true });
  return () => source.removeEventListener("abort", abort);
}

function isAbortError(error: unknown): boolean {
  return error instanceof Error && error.name === "AbortError";
}

function unrefTimer(timer: ReturnType<typeof setTimeout>): void {
  const candidate = timer as unknown as { unref?: () => void };
  candidate.unref?.();
}

function validateLimit(limit: number): void {
  if (!Number.isSafeInteger(limit) || limit < 1) throw new RangeError("limit must be a positive integer");
}

function compareReplicaRecords(left: ReplicaRecord, right: ReplicaRecord): number {
  return stableJson(left.primaryKey).localeCompare(stableJson(right.primaryKey));
}

function sortPending(values: Iterable<PendingMutation>): PendingMutation[] {
  return [...values].sort((left, right) =>
    left.enqueuedAtMs - right.enqueuedAtMs || left.mutation_id.localeCompare(right.mutation_id));
}

function sortRejected(values: Iterable<RejectedMutation>): RejectedMutation[] {
  return [...values].sort((left, right) =>
    left.rejectedAtMs - right.rejectedAtMs || left.mutation_id.localeCompare(right.mutation_id));
}
