import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";

import { FFDBClient, MemorySessionStore, generateId } from "@ffdb/client";
import { OfflineSyncClient } from "@ffdb/sync-client";
import { NodeSQLiteReplica } from "@ffdb/sync-client/node";

const apiUrl = required("FFDB_API_URL");
const projectId = required("VITE_FFDB_PROJECT_ID");
const email = required("FFDB_TEST_EMAIL");
const password = required("FFDB_TEST_PASSWORD");
const dataDirectory = resolve(".data");
await mkdir(dataDirectory, { recursive: true, mode: 0o700 });

const api = new FFDBClient({
  baseUrl: apiUrl,
  projectId,
  sessionStore: new MemorySessionStore(`field-notes-node-${projectId}`),
});
const session = await api.auth.signIn(email, password);
const replicaPath = resolve(dataDirectory, `field-notes-${projectId}-${session.user.id}.sqlite3`);
const replica = new NodeSQLiteReplica(replicaPath);
const sync = new OfflineSyncClient(api, replica);
const taskId = generateId("node_");

try {
  await sync.sync();
  await sync.mutate({
    mutation_id: generateId("mutation_"),
    table: "field_tasks",
    primary_key: taskId,
    operation: "insert",
    values: {
      id: taskId,
      owner_id: session.user.id,
      title: "Verify the Node SQLite replica",
      notes: "Created by examples/field-notes/scripts/node-smoke.mjs",
      status: "open",
      priority: "medium",
      attachment_count: 0,
      created_at_ms: Date.now(),
      updated_at_ms: Date.now(),
    },
    base_row_version: null,
    client_timestamp_ms: Date.now(),
  });

  const optimistic = await sync.getRow("field_tasks", taskId);
  if (optimistic === null) throw new Error("Optimistic Node replica insert was not visible");
  await sync.sync();

  const remote = await api.query({
    sql: "SELECT id, title FROM field_tasks WHERE id = ?1 AND owner_id = auth.uid()",
    parameters: [{ type: "text", value: taskId }],
  });
  if (remote.rows.length !== 1) throw new Error("Node mutation did not reach FFDB");

  await sync.mutate({
    mutation_id: generateId("mutation_"),
    table: "field_tasks",
    primary_key: taskId,
    operation: "delete",
    values: null,
    base_row_version: (await sync.getRow("field_tasks", taskId))?.rowVersion ?? null,
    client_timestamp_ms: Date.now(),
  });
  await sync.sync();

  console.log(JSON.stringify({
    ready: true,
    runtime: "node",
    adapter: "NodeSQLiteReplica",
    optimisticInsert: true,
    pushedToServer: true,
    cleanupDelete: true,
    replicaPath,
  }, null, 2));
} finally {
  await replica.close();
  await api.auth.signOut().catch(() => undefined);
}

function required(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required; copy .env.example to .env.local`);
  return value;
}
