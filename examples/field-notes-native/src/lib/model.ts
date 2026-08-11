import type { ReplicaRecord } from "@ffdb/sync-client";

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
  readonly updatedAtMs: number;
  readonly rowVersion: number;
  readonly serverSequence: number;
}

export function tasksFromReplica(records: readonly ReplicaRecord[]): readonly FieldTask[] {
  return records.map<FieldTask>((record) => {
    const values = record.values;
    return {
      id: stringValue(values.id, "id"),
      ownerId: stringValue(values.owner_id, "owner_id"),
      title: stringValue(values.title, "title"),
      notes: stringValue(values.notes, "notes"),
      status: values.status === "done" ? "done" : "open",
      priority: values.priority === "low" || values.priority === "high" ? values.priority : "medium",
      attachmentCount: numberValue(values.attachment_count, "attachment_count"),
      updatedAtMs: numberValue(values.updated_at_ms, "updated_at_ms"),
      rowVersion: record.rowVersion,
      serverSequence: record.serverSequence,
    };
  }).sort((left, right) => right.updatedAtMs - left.updatedAtMs);
}

function stringValue(value: unknown, name: string): string {
  if (typeof value !== "string") throw new TypeError(`Replica ${name} is invalid`);
  return value;
}

function numberValue(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new TypeError(`Replica ${name} is invalid`);
  }
  return value;
}
