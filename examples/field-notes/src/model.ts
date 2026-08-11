import type { FFDBClient, QueryResult, ResultCell, StorageObjectItem } from "@ffdb/client";
import { generateId } from "@ffdb/client";
import type { OfflineSyncClient, ReplicaRecord } from "@ffdb/sync-client";

export type TaskStatus = "open" | "done";
export type TaskPriority = "low" | "medium" | "high";

export interface FieldTask {
  readonly id: string;
  readonly ownerId: string;
  readonly title: string;
  readonly notes: string;
  readonly status: TaskStatus;
  readonly priority: TaskPriority;
  readonly attachmentCount: number;
  readonly createdAtMs: number;
  readonly updatedAtMs: number;
  readonly rowVersion: number;
  readonly serverSequence: number;
}

export interface TaskEvent {
  readonly id: string;
  readonly kind: string;
  readonly message: string;
  readonly createdAtMs: number;
}

export interface FeatureCheck {
  readonly id: "auth" | "sql" | "transactions" | "rls" | "sync" | "storage" | "sessions";
  readonly label: string;
  readonly status: "ready" | "pending" | "error";
  readonly detail: string;
}

export type TaskPersistenceState = "local" | "confirmed" | "last_confirmed";

export function taskPersistenceState(
  task: FieldTask,
  pending: boolean,
  serverReady: boolean,
): TaskPersistenceState {
  if (pending || task.serverSequence < 0) return "local";
  return serverReady ? "confirmed" : "last_confirmed";
}

export const featureLabels: Readonly<Record<FeatureCheck["id"], string>> = {
  auth: "Authentication",
  sql: "Parameterized SQL",
  transactions: "Transactions",
  rls: "Row-level security",
  sync: "Offline sync",
  storage: "Object storage",
  sessions: "Session management",
};

export function taskFromReplica(record: ReplicaRecord): FieldTask {
  const values = record.values;
  return {
    id: asString(values.id, "id"),
    ownerId: asString(values.owner_id, "owner_id"),
    title: asString(values.title, "title"),
    notes: asString(values.notes, "notes"),
    status: asTaskStatus(values.status),
    priority: asTaskPriority(values.priority),
    attachmentCount: asNumber(values.attachment_count, "attachment_count"),
    createdAtMs: asNumber(values.created_at_ms, "created_at_ms"),
    updatedAtMs: asNumber(values.updated_at_ms, "updated_at_ms"),
    rowVersion: record.rowVersion,
    serverSequence: record.serverSequence,
  };
}

export function tasksFromReplica(records: readonly ReplicaRecord[]): readonly FieldTask[] {
  return records.map(taskFromReplica).sort((left, right) => right.updatedAtMs - left.updatedAtMs);
}

export function eventsFromResult(result: QueryResult | null): readonly TaskEvent[] {
  if (result === null) return [];
  return result.rows.map((row) => ({
    id: cellString(row[0], "event id"),
    kind: cellString(row[1], "event kind"),
    message: cellString(row[2], "event message"),
    createdAtMs: cellNumber(row[3], "event timestamp"),
  }));
}

export function filterTasks(
  tasks: readonly FieldTask[],
  filter: "all" | TaskStatus,
  search: string,
): readonly FieldTask[] {
  const normalized = search.trim().toLocaleLowerCase();
  return tasks.filter((task) => {
    if (filter !== "all" && task.status !== filter) return false;
    return normalized === "" || `${task.title} ${task.notes}`.toLocaleLowerCase().includes(normalized);
  });
}

export async function seedWorkspace(client: FFDBClient, ownerId: string): Promise<boolean> {
  const existing = await client.query({
    sql: "SELECT id FROM field_tasks WHERE owner_id = auth.uid() LIMIT 1",
  });
  if (existing.rows.length > 0) return false;

  const now = Date.now();
  const seeds = [
    { title: "Document the sync boundary", priority: "high", notes: "Queue one edit locally, then inspect the push and pull phases." },
    { title: "Upload launch brief", priority: "medium", notes: "Attach a file and verify the protected object metadata." },
    { title: "Verify RLS with a second user", priority: "high", notes: "Create another account and confirm this row disappears." },
    { title: "Define initial task schema", priority: "low", notes: "Applied by the trusted setup script.", status: "done" },
  ] as const;

  const statements = seeds.flatMap((seed, index) => {
      const taskId = generateId("task_");
      const createdAt = now - (seeds.length - index) * 60_000;
      return [
        {
          sql: "INSERT INTO field_tasks (id, owner_id, title, notes, status, priority, attachment_count, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7)",
          parameters: [
            { type: "text" as const, value: taskId },
            { type: "text" as const, value: ownerId },
            { type: "text" as const, value: seed.title },
            { type: "text" as const, value: seed.notes },
            { type: "text" as const, value: "status" in seed ? seed.status : "open" },
            { type: "text" as const, value: seed.priority },
            { type: "integer" as const, value: createdAt },
          ],
        },
        {
          sql: "INSERT INTO field_task_events (id, task_id, owner_id, kind, message, created_at_ms) VALUES (?1, ?2, ?3, 'created', 'Seeded in one atomic transaction', ?4)",
          parameters: [
            { type: "text" as const, value: generateId("event_") },
            { type: "text" as const, value: taskId },
            { type: "text" as const, value: ownerId },
            { type: "integer" as const, value: createdAt },
          ],
        },
      ];
    });
  try {
    await client.transaction({ statements });
  } catch (cause) {
    // React StrictMode may begin the development initializer twice. If the
    // other run committed first, treat this run as an idempotent no-op; do not
    // hide a real migration, policy, or transaction failure.
    const raced = await client.query({
      sql: "SELECT id FROM field_tasks WHERE owner_id = auth.uid() LIMIT 1",
    }).catch(() => null);
    if (raced === null || raced.rows.length === 0) throw cause;
    return false;
  }
  return true;
}

export async function createTask(
  client: FFDBClient,
  ownerId: string,
  title: string,
  priority: TaskPriority,
): Promise<string> {
  const taskId = generateId("task_");
  const now = Date.now();
  await client.transaction({
    statements: [
      {
        sql: "INSERT INTO field_tasks (id, owner_id, title, notes, status, priority, attachment_count, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, '', 'open', ?4, 0, ?5, ?5)",
        parameters: [
          { type: "text", value: taskId },
          { type: "text", value: ownerId },
          { type: "text", value: title.trim() },
          { type: "text", value: priority },
          { type: "integer", value: now },
        ],
      },
      eventStatement(taskId, ownerId, "created", "Task and audit event committed atomically", now),
    ],
  });
  return taskId;
}

export async function toggleTask(client: FFDBClient, task: FieldTask): Promise<void> {
  const nextStatus: TaskStatus = task.status === "open" ? "done" : "open";
  const now = Date.now();
  await client.transaction({
    statements: [
      {
        sql: "UPDATE field_tasks SET status = ?1, updated_at_ms = ?2 WHERE id = ?3 AND owner_id = auth.uid()",
        parameters: [
          { type: "text", value: nextStatus },
          { type: "integer", value: now },
          { type: "text", value: task.id },
        ],
      },
      eventStatement(task.id, task.ownerId, "status", nextStatus === "done" ? "Task completed" : "Task reopened", now),
    ],
  });
}

export async function queueTaskEdit(
  sync: OfflineSyncClient,
  task: FieldTask,
  patch: { readonly title?: string; readonly notes?: string; readonly priority?: TaskPriority },
): Promise<void> {
  await sync.mutate({
    mutation_id: generateId("mutation_"),
    table: "field_tasks",
    primary_key: task.id,
    operation: "update",
    values: { ...patch, updated_at_ms: Date.now() },
    base_row_version: task.rowVersion,
    client_timestamp_ms: Date.now(),
  });
}

export async function queueTaskDelete(sync: OfflineSyncClient, task: FieldTask): Promise<void> {
  await sync.mutate({
    mutation_id: generateId("mutation_"),
    table: "field_tasks",
    primary_key: task.id,
    operation: "delete",
    values: null,
    base_row_version: task.rowVersion,
    client_timestamp_ms: Date.now(),
  });
}

export function taskObjectPrefix(userId: string, taskId: string): string {
  return `users/${userId}/tasks/${taskId}/`;
}

export function objectDisplayName(object: StorageObjectItem): string {
  const leaf = object.object_key.split("/").at(-1) ?? object.object_key;
  return leaf.replace(/^file_[A-Za-z0-9-]+-/, "");
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function safeFileName(name: string): string {
  const normalized = name.trim().replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "");
  return normalized.slice(0, 120) || "attachment";
}

export function formatTime(timestamp: number | null): string {
  if (timestamp === null) return "Not yet";
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" }).format(timestamp);
}

function eventStatement(taskId: string, ownerId: string, kind: string, message: string, timestamp: number) {
  return {
    sql: "INSERT INTO field_task_events (id, task_id, owner_id, kind, message, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    parameters: [
      { type: "text" as const, value: generateId("event_") },
      { type: "text" as const, value: taskId },
      { type: "text" as const, value: ownerId },
      { type: "text" as const, value: kind },
      { type: "text" as const, value: message },
      { type: "integer" as const, value: timestamp },
    ],
  };
}

function cellString(cell: ResultCell | undefined, name: string): string {
  if (typeof cell !== "string") throw new TypeError(`${name} must be text`);
  return cell;
}

function cellNumber(cell: ResultCell | undefined, name: string): number {
  if (typeof cell !== "number") throw new TypeError(`${name} must be numeric`);
  return cell;
}

function asString(value: unknown, name: string): string {
  if (typeof value !== "string") throw new TypeError(`${name} must be text`);
  return value;
}

function asNumber(value: unknown, name: string): number {
  if (typeof value !== "number") throw new TypeError(`${name} must be numeric`);
  return value;
}

function asTaskStatus(value: unknown): TaskStatus {
  if (value !== "open" && value !== "done") throw new TypeError("status is invalid");
  return value;
}

function asTaskPriority(value: unknown): TaskPriority {
  if (value !== "low" && value !== "medium" && value !== "high") throw new TypeError("priority is invalid");
  return value;
}
