import { describe, expect, it } from "vitest";
import type { ReplicaRecord } from "@ffdb/sync-client";

import { filterTasks, formatBytes, safeFileName, taskFromReplica, taskObjectPrefix } from "./model";

const record: ReplicaRecord = {
  table: "field_tasks",
  primaryKey: "task_1",
  values: {
    id: "task_1",
    owner_id: "user_1",
    title: "Verify the sync boundary",
    notes: "Queue one local change",
    status: "open",
    priority: "high",
    attachment_count: 2,
    created_at_ms: 100,
    updated_at_ms: 200,
  },
  rowVersion: 3,
  serverSequence: 8,
};

describe("field notes model", () => {
  it("maps a replica row without dropping authoritative metadata", () => {
    expect(taskFromReplica(record)).toMatchObject({
      id: "task_1",
      ownerId: "user_1",
      status: "open",
      priority: "high",
      rowVersion: 3,
      serverSequence: 8,
    });
  });

  it("filters by status and searches title plus notes", () => {
    const task = taskFromReplica(record);
    expect(filterTasks([task], "open", "local")).toEqual([task]);
    expect(filterTasks([task], "done", "")).toEqual([]);
    expect(filterTasks([task], "all", "missing")).toEqual([]);
  });

  it("scopes storage keys to both the user and task", () => {
    expect(taskObjectPrefix("user_1", "task_1")).toBe("users/user_1/tasks/task_1/");
    expect(safeFileName("../../launch brief (final).pdf")).toBe("..-..-launch-brief-final-.pdf");
  });

  it("formats bounded object sizes", () => {
    expect(formatBytes(800)).toBe("800 B");
    expect(formatBytes(2_048)).toBe("2 KB");
    expect(formatBytes(1_572_864)).toBe("1.5 MB");
  });
});
