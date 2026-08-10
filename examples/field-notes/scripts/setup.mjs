import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { FFDBClient, FFDBError } from "@ffdb/client";
import { loadMigration } from "@ffdb/cli";

const apiUrl = required("FFDB_API_URL");
const projectId = required("VITE_FFDB_PROJECT_ID");
const developerKey = required("FFDB_DEVELOPER_KEY");
const client = new FFDBClient({ baseUrl: apiUrl, projectId, developerKey });

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const migrationPath = resolve(root, "migrations/20260810_field_notes.sql");

await client.readiness();
const migration = await loadMigration(migrationPath, Date.UTC(2026, 7, 10));
await client.migrate(migration);

const buckets = await client.storage.buckets();
let bucket = buckets.find((candidate) => candidate.name === "field-notes");
if (bucket === undefined) {
  try {
    bucket = await createFieldNotesBucket(true);
  } catch (cause) {
    if (!(cause instanceof FFDBError) || cause.code !== "storage.versioning_not_configured") throw cause;
    bucket = await createFieldNotesBucket(false);
  }
}

const [schema, policies, integrity] = await Promise.all([
  client.schema(),
  client.policies(),
  client.integrityCheck(),
]);

const expectedTables = ["field_tasks", "field_task_events"];
const missingTables = expectedTables.filter((name) => !schema.tables.some((table) => table.name === name));
const expectedPolicies = [
  "field_tasks_owner",
  "field_task_events_owner",
  "field_notes_buckets_authenticated",
  "field_notes_objects_owner",
  "field_notes_uploads_owner",
  "field_notes_versions_owner",
];
const missingPolicies = expectedPolicies.filter((name) => !policies.some((policy) => policy.name === name));
if (missingTables.length > 0 || missingPolicies.length > 0 || !integrity.ok) {
  throw new Error(JSON.stringify({ missingTables, missingPolicies, integrity: integrity.messages }));
}

console.log(JSON.stringify({
  ready: true,
  projectId,
  schemaVersion: schema.version,
  tables: expectedTables,
  policies: expectedPolicies.length,
  bucket: { name: bucket.name, versioning: bucket.versioning },
  limitations: bucket.versioning ? [] : ["Storage provider bucket versioning is not configured."],
  integrity: integrity.ok,
}, null, 2));

function createFieldNotesBucket(versioning) {
  return client.storage.createBucket({
    name: "field-notes",
    public: false,
    max_object_bytes: 50 * 1024 * 1024,
    versioning,
  });
}

function required(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required; copy .env.example to .env.local`);
  return value;
}
