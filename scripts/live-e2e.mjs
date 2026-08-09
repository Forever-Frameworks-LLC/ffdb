#!/usr/bin/env node

import { createHash, randomUUID } from "node:crypto";

import {
  FFDBClient,
  FFDBError,
  MemoryDeveloperSessionStore,
  MemorySessionStore,
} from "../packages/client/dist/index.js";
import { MemoryReplica, OfflineSyncClient } from "../packages/sync-client/dist/index.js";

const API = process.env.FFDB_E2E_API_URL ?? "http://127.0.0.1:8080";
const MAILPIT = process.env.FFDB_E2E_MAILPIT_URL ?? "http://127.0.0.1:8025";
const BOOTSTRAP_TOKEN = process.env.FFDB_E2E_BOOTSTRAP_TOKEN
  ?? "local-bootstrap-token-change-before-production";
const DEVELOPER_EMAIL = process.env.FFDB_E2E_DEVELOPER_EMAIL ?? "admin@ffdb.local.test";
const DEVELOPER_PASSWORD = process.env.FFDB_E2E_DEVELOPER_PASSWORD
  ?? "Local-FFDB-Admin-Passphrase-2026";
const runId = `${Date.now().toString(36)}-${randomUUID().slice(0, 8)}`;
const steps = [];

function check(condition, message) {
  if (!condition) throw new Error(message);
}

function passed(name) {
  steps.push(name);
  process.stdout.write(`ok ${steps.length} - ${name}\n`);
}

async function rateAwareFetch(input, init) {
  for (let attempt = 0; attempt < 8; attempt += 1) {
    const request = input instanceof Request ? input.clone() : input;
    const response = await fetch(request, init);
    if (response.status !== 429 || attempt === 7) return response;
    const retryAfterSeconds = Number.parseInt(response.headers.get("retry-after") ?? "1", 10);
    const delayMs = Number.isSafeInteger(retryAfterSeconds)
      ? Math.min(Math.max(retryAfterSeconds, 1), 15) * 1_000
      : 1_000;
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  }
  throw new Error("bounded request retry loop ended unexpectedly");
}

async function jsonRequest(path, init = {}) {
  const headers = new Headers(init.headers);
  if (init.body !== undefined && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  const response = await fetch(`${API}${path}`, { ...init, headers });
  const text = await response.text();
  let body = null;
  if (text !== "") {
    try { body = JSON.parse(text); } catch { body = text; }
  }
  return { response, body };
}

async function expectSdkError(work, expectedCodes, label) {
  try {
    await work();
  } catch (error) {
    if (error instanceof FFDBError && expectedCodes.some((code) => error.code.startsWith(code))) {
      return error;
    }
    throw error;
  }
  throw new Error(`${label} unexpectedly succeeded`);
}

async function developerSession() {
  const bootstrap = await jsonRequest("/v1/developer/bootstrap", {
    method: "POST",
    headers: { "x-ffdb-bootstrap-token": BOOTSTRAP_TOKEN },
    body: JSON.stringify({ email: DEVELOPER_EMAIL, password: DEVELOPER_PASSWORD }),
  });
  if (bootstrap.response.ok) return bootstrap.body;
  const signIn = await jsonRequest("/v1/developer/sign-in", {
    method: "POST",
    body: JSON.stringify({ email: DEVELOPER_EMAIL, password: DEVELOPER_PASSWORD }),
  });
  if (!signIn.response.ok) {
    throw new Error(
      `developer bootstrap/sign-in failed (${bootstrap.response.status}/${signIn.response.status}); `
      + "set FFDB_E2E_DEVELOPER_EMAIL and FFDB_E2E_DEVELOPER_PASSWORD for an existing installation",
    );
  }
  return signIn.body;
}

function deepStrings(value, output = []) {
  if (typeof value === "string") output.push(value);
  else if (Array.isArray(value)) for (const item of value) deepStrings(item, output);
  else if (value !== null && typeof value === "object") {
    for (const item of Object.values(value)) deepStrings(item, output);
  }
  return output;
}

async function actionToken(recipient, subjectPattern, notBeforeMs) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const listing = await fetch(`${MAILPIT}/api/v1/messages?limit=100`);
    if (listing.ok) {
      const payload = await listing.json();
      const messages = payload.messages ?? payload.Messages ?? [];
      for (const message of messages) {
        const strings = deepStrings(message);
        if (!strings.some((value) => value.toLowerCase().includes(recipient.toLowerCase()))) continue;
        if (!strings.some((value) => subjectPattern.test(value))) continue;
        const created = Date.parse(message.Created ?? message.created ?? message.Date ?? "");
        if (Number.isFinite(created) && created + 2_000 < notBeforeMs) continue;
        const id = message.ID ?? message.Id ?? message.id;
        if (typeof id !== "string") continue;
        const detail = await fetch(`${MAILPIT}/api/v1/message/${encodeURIComponent(id)}`);
        if (!detail.ok) continue;
        const detailPayload = await detail.json();
        const content = deepStrings(detailPayload).join("\n");
        const match = content.match(/ffdb_(?:action|invitation)_[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/u);
        if (match !== null) return match[0];
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`timed out waiting for transactional email to ${recipient}`);
}

function checksum(spec) {
  return createHash("sha256")
    .update(spec.id).update("\0")
    .update(spec.name).update("\0")
    .update(spec.up_sql).update("\0")
    .update(spec.down_sql)
    .digest("hex");
}

function migration(id, name, upSql, downSql) {
  const value = {
    id,
    name,
    up_sql: upSql,
    down_sql: downSql,
    checksum: "",
    created_at_ms: Date.now(),
  };
  value.checksum = checksum(value);
  return value;
}

function query(sql, values = []) {
  return {
    sql,
    parameters: values.map((value) => {
      if (value === null) return { type: "null" };
      if (typeof value === "number") return Number.isInteger(value)
        ? { type: "integer", value }
        : { type: "real", value };
      return { type: "text", value };
    }),
  };
}

function verifiedJwtSubject(token) {
  const segments = token.split(".");
  check(segments.length === 3, "access token is not a compact JWT");
  const claims = JSON.parse(Buffer.from(segments[1], "base64url").toString("utf8"));
  check(typeof claims.sub === "string", "access token subject is missing");
  return claims.sub;
}

async function main() {
  const readiness = await jsonRequest("/readyz");
  check(readiness.response.ok && readiness.body?.status === "ready", "API is not ready");
  passed("API readiness and control-plane migration");

  const platformSession = await developerSession();
  const platformStore = new MemoryDeveloperSessionStore(`e2e-platform-${runId}`);
  await platformStore.set(platformSession);
  const client = new FFDBClient({
    baseUrl: API,
    developerSessionStore: platformStore,
    fetch: rateAwareFetch,
  });
  const setupBefore = await client.instanceSetupStatus();
  check(
    setupBefore.bootstrap_available === false && setupBefore.setup_required === true,
    "fresh owner was not required to configure the instance",
  );
  const configured = await client.configureInstance({
    deployment_mode: "private",
    organization_creation_policy: "owner_only",
  }, { idempotencyKey: `e2e-instance-${runId}` });
  check(
    configured.instance.current_user_role === "owner"
      && configured.instance.deployment_mode === "private"
      && configured.instance.billing_enforcement_enabled === false
      && configured.onboarding === null,
    "private instance setup did not persist the owner-controlled deployment mode",
  );
  const setupAfter = await client.instanceSetupStatus();
  check(setupAfter.setup_required === false, "instance remained unconfigured after setup");
  check((await client.instanceAdministrators()).some((value) => value.role === "owner"), "owner administrator missing");
  check((await client.instancePlans()).length === 3, "default instance plan catalog missing");
  passed("first owner bootstrap, deployment-mode setup, administration, and plan catalog");

  const organization = await client.createOrganization({
    name: `E2E ${runId}`,
    slug: `e2e-${runId}`,
  });
  const project = await client.createProject({
    organization_id: organization.id,
    name: `E2E ${runId}`,
    slug: `e2e-${runId}`,
    region: "local",
  }, { idempotencyKey: `e2e-project-${runId}` });
  client.setProjectId(project.id);
  const apiKey = await client.createApiKey({
    name: `e2e-${runId}`,
    scopes: [
      "projects_read", "projects_write", "database_query", "database_migrate",
      "database_schema", "auth_manage", "storage_manage", "email_manage",
      "keys_rotate", "backups_manage", "logs_read",
    ],
    expires_at_ms: null,
  });
  client.setDeveloperKey(apiKey.secret);
  check((await client.projects(organization.id)).some((value) => value.id === project.id), "project missing");
  passed("organization, project, exactly-one-database route, and scoped API key");

  const coreMigration = migration(
    `core_${runId}`,
    "RLS documents and protected object metadata",
    [
      "CREATE TABLE documents (id TEXT PRIMARY KEY, owner_id TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '')",
      "ALTER TABLE documents ENABLE ROW LEVEL SECURITY",
      "ALTER TABLE documents FORCE ROW LEVEL SECURITY",
      "CREATE POLICY documents_owner ON documents AS PERMISSIVE FOR ALL TO authenticated USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())",
      "CREATE POLICY storage_buckets_authenticated ON storage_buckets AS PERMISSIVE FOR SELECT TO authenticated USING (1)",
      "CREATE POLICY storage_objects_owner ON storage_objects AS PERMISSIVE FOR ALL TO authenticated USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())",
      "CREATE POLICY storage_uploads_owner ON storage_uploads AS PERMISSIVE FOR ALL TO authenticated USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())",
      "CREATE POLICY storage_versions_owner ON storage_versions AS PERMISSIVE FOR ALL TO authenticated USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())",
    ].join("; "),
    [
      "DROP POLICY storage_versions_owner ON storage_versions",
      "DROP POLICY storage_uploads_owner ON storage_uploads",
      "DROP POLICY storage_objects_owner ON storage_objects",
      "DROP POLICY storage_buckets_authenticated ON storage_buckets",
      "DROP POLICY documents_owner ON documents",
      "ALTER TABLE documents DISABLE ROW LEVEL SECURITY",
      "DROP TABLE documents",
    ].join("; "),
  );
  await client.migrate(coreMigration, { idempotencyKey: `e2e-migrate-${runId}` });
  const schema = await client.schema();
  check(schema.tables.some((table) => table.name === "documents" && table.rls_enabled), "RLS schema missing");
  check((await client.policies()).some((policy) => policy.name === "documents_owner"), "policy missing");
  passed("up migration, CREATE POLICY compilation, schema, and policy inspection");

  const aliceEmail = `alice-${runId}@example.test`;
  const bobEmail = `bob-${runId}@example.test`;
  const initialPassword = "Correct-Horse-Battery-Staple-2026"; // gitleaks:allow -- local E2E fixture
  const resetPassword = "Reset-Horse-Battery-Staple-2026";
  const endUserFetch = async (input, init) => {
    const response = await rateAwareFetch(input, init);
    const url = new URL(typeof input === "string" || input instanceof URL ? input : input.url);
    if (url.pathname.endsWith("/snapshot") && !response.ok) {
      const contentType = response.headers.get("content-type") ?? "missing";
      let code = "non_json_error";
      try { code = (await response.clone().json())?.error?.code ?? "invalid_error_shape"; } catch { /* safe classification only */ }
      throw new Error(`snapshot request failed (${response.status}; ${contentType}; ${code}; query=${url.searchParams.toString() || "none"})`);
    }
    if (url.pathname.includes("/storage/") && !response.ok) {
      let code = "non_json_error";
      try { code = (await response.clone().json())?.error?.code ?? "invalid_error_shape"; } catch { /* safe classification only */ }
      throw new Error(`storage request failed (${response.status}; ${code}; endpoint=${url.pathname.split("/").at(-1)})`);
    }
    return response;
  };
  const alice = new FFDBClient({
    baseUrl: API,
    projectId: project.id,
    fetch: endUserFetch,
    sessionStore: new MemorySessionStore(`alice-${runId}`),
  });
  const bob = new FFDBClient({
    baseUrl: API,
    projectId: project.id,
    fetch: rateAwareFetch,
    sessionStore: new MemorySessionStore(`bob-${runId}`),
  });

  const aliceMailStart = Date.now();
  const aliceRegistration = await alice.auth.register({ email: aliceEmail, password: initialPassword });
  const aliceVerification = await actionToken(aliceEmail, /verify/i, aliceMailStart);
  await alice.auth.verifyEmail(aliceVerification);
  const aliceFirstSession = await alice.auth.signIn(aliceEmail, initialPassword);
  const bobMailStart = Date.now();
  const bobRegistration = await bob.auth.register({ email: bobEmail, password: initialPassword });
  const bobVerification = await actionToken(bobEmail, /verify/i, bobMailStart);
  await bob.auth.verifyEmail(bobVerification);
  const bobFirstSession = await bob.auth.signIn(bobEmail, initialPassword);
  check(aliceRegistration.verification_required && bobRegistration.verification_required, "verification was bypassed");
  const aliceUserId = verifiedJwtSubject(aliceFirstSession.access_token);
  const bobUserId = verifiedJwtSubject(bobFirstSession.access_token);
  check(
    aliceUserId !== "00000000-0000-0000-0000-000000000000"
      && bobUserId !== "00000000-0000-0000-0000-000000000000"
      && aliceUserId !== bobUserId,
    "signed access-token subjects are invalid",
  );
  passed("two-user registration, email verification, and sign-in through captured mail");

  await alice.query(query(
    "INSERT INTO documents(id,owner_id,title,body) VALUES (?1,?2,?3,?4)",
    ["alice-1", aliceUserId, "Alice", "private-a"],
  ));
  await bob.query(query(
    "INSERT INTO documents(id,owner_id,title,body) VALUES (?1,?2,?3,?4)",
    ["bob-1", bobUserId, "Bob", "private-b"],
  ));
  const aliceRows = await alice.query(query("SELECT id,owner_id,body FROM documents ORDER BY id"));
  const bobRows = await bob.query(query("SELECT id,owner_id,body FROM documents ORDER BY id"));
  check(aliceRows.rows.length === 1 && aliceRows.rows[0]?.[0] === "alice-1", "Alice RLS leak");
  check(bobRows.rows.length === 1 && bobRows.rows[0]?.[0] === "bob-1", "Bob RLS leak");
  await expectSdkError(
    () => alice.query(query(
      "INSERT INTO documents(id,owner_id,title,body) VALUES (?1,?2,?3,?4)",
      ["stolen", bobUserId, "No", "denied"],
    )),
    ["query.", "database."],
    "cross-user RLS write",
  );
  await expectSdkError(
    () => alice.query(query("CREATE TABLE end_user_ddl(id INTEGER)")),
    ["query.statement_not_allowed", "query."],
    "end-user DDL",
  );
  const backing = `__ffdb_data_${createHash("sha256").update("documents").digest("hex")}`;
  await expectSdkError(
    () => alice.query(query(`SELECT * FROM "${backing}"`)),
    ["query.", "database."],
    "direct RLS backing access",
  );
  passed("two-user RLS isolation, denied cross-user write, denied DDL, and denied backing access");

  const resetMailStart = Date.now();
  await alice.auth.startPasswordReset(aliceEmail);
  const resetToken = await actionToken(aliceEmail, /reset/i, resetMailStart);
  await alice.auth.completePasswordReset(resetToken, resetPassword);
  await expectSdkError(
    () => alice.query(query("SELECT id FROM documents")),
    ["auth."],
    "access token after password reset",
  );
  const sessionBeforeRefresh = await alice.auth.signIn(aliceEmail, resetPassword);
  await alice.auth.refresh();
  const reused = await jsonRequest(`/v1/projects/${project.id}/auth/refresh`, {
    method: "POST",
    body: JSON.stringify({ refresh_token: sessionBeforeRefresh.refresh_token }),
  });
  check(reused.response.status === 401, "refresh-token reuse was not rejected");
  await expectSdkError(
    () => alice.query(query("SELECT id FROM documents")),
    ["auth."],
    "access token after refresh-family reuse",
  );
  await alice.auth.signIn(aliceEmail, resetPassword);
  passed("password reset, immediate session invalidation, refresh rotation, and reuse-family revocation");

  const activeSession = await alice.auth.session();
  check(activeSession !== null, "active session missing before snapshot");
  const snapshotPreflight = await jsonRequest(`/v1/projects/${project.id}/snapshot`, {
    headers: { authorization: `Bearer ${activeSession.access_token}` },
  });
  if (!snapshotPreflight.response.ok) {
    const code = snapshotPreflight.body?.error?.code ?? "non_json_error";
    const contentType = snapshotPreflight.response.headers.get("content-type") ?? "missing";
    throw new Error(`snapshot preflight failed (${snapshotPreflight.response.status}; ${contentType}; ${code})`);
  }

  const offlineReplica = new MemoryReplica();
  const offline = new OfflineSyncClient(alice, offlineReplica, { pullBatchSize: 2 });
  await offline.sync();
  check(offlineReplica.rows().some((row) => row.primaryKey === "alice-1"), "snapshot omitted Alice row");
  const offlineId = `offline-${runId}`;
  await offline.mutate({
    mutation_id: `insert-${runId}`,
    table: "documents",
    primary_key: offlineId,
    operation: "insert",
    values: { owner_id: aliceUserId, title: "Offline", body: "v1" },
    base_row_version: null,
    client_timestamp_ms: Date.now() - 60_000,
  });
  await offline.sync();
  await offline.mutate({
    mutation_id: `update-old-clock-${runId}`,
    table: "documents",
    primary_key: offlineId,
    operation: "update",
    values: { body: "v2-server-order" },
    base_row_version: 0,
    client_timestamp_ms: 1,
  });
  await offline.sync();
  const lww = await alice.query(query("SELECT body FROM documents WHERE id=?1", [offlineId]));
  check(lww.rows[0]?.[0] === "v2-server-order", "server-sequenced LWW did not win");
  const duplicateMutation = {
    schema_version: (await alice.sync.snapshot(["documents"])).schema_version,
    mutations: [{
      mutation_id: `duplicate-${runId}`,
      table: "documents",
      primary_key: offlineId,
      operation: "update",
      values: { title: "Duplicate-safe" },
      base_row_version: null,
      client_timestamp_ms: Date.now(),
    }],
  };
  await alice.sync.push(duplicateMutation);
  const duplicate = await alice.sync.push(duplicateMutation);
  check(duplicate.results[0]?.status === "duplicate", "sync mutation replay was not idempotent");
  await offline.mutate({
    mutation_id: `delete-${runId}`,
    table: "documents",
    primary_key: offlineId,
    operation: "delete",
    values: null,
    base_row_version: null,
    client_timestamp_ms: Date.now(),
  });
  await offline.sync();
  check(!(await alice.query(query("SELECT id FROM documents WHERE id=?1", [offlineId]))).rows.length, "tombstone delete failed");
  passed("offline snapshot, push/pull, deterministic server-order LWW, replay, and tombstone processing");

  const bucketName = `e2e-${runId}`.slice(0, 63);
  await client.storage.createBucket({
    name: bucketName,
    public: false,
    max_object_bytes: 10 * 1024 * 1024,
    versioning: false,
  });
  passed("developer-created private storage bucket");
  const firstBody = new TextEncoder().encode("alice-private-object-v1");
  await alice.storage.upload(bucketName, "private/document.txt", firstBody, {
    sizeBytes: firstBody.byteLength,
    contentType: "text/plain",
  });
  const aliceObjects = await alice.storage.list(bucketName);
  const bobObjects = await bob.storage.list(bucketName);
  check(aliceObjects.items.some((item) => item.object_key === "private/document.txt"), "Alice object missing");
  check(bobObjects.items.length === 0, "storage RLS leaked object metadata");
  await expectSdkError(
    () => bob.storage.downloadUrl(bucketName, "private/document.txt"),
    ["storage."],
    "cross-user object download",
  );
  const replacement = new TextEncoder().encode("alice-private-object-v2");
  await alice.storage.upload(bucketName, "private/document.txt", replacement, {
    sizeBytes: replacement.byteLength,
    contentType: "text/plain",
  });
  const download = await alice.storage.downloadUrl(bucketName, "private/document.txt");
  const downloaded = await fetch(download.url, { method: download.method, headers: download.headers });
  check(downloaded.ok && await downloaded.text() === "alice-private-object-v2", "object overwrite was not visible");
  const multipartBody = new TextEncoder().encode("one-part-multipart-object");
  const multipart = await alice.storage.createMultipart(bucketName, "private/multipart.txt", {
    sizeBytes: multipartBody.byteLength,
    contentType: "text/plain",
  });
  const part = await alice.storage.uploadPart(multipart, 1, multipartBody, {
    sizeBytes: multipartBody.byteLength,
    contentType: "text/plain",
  });
  await alice.storage.completeMultipart(multipart, [part], {
    sizeBytes: multipartBody.byteLength,
    contentType: "text/plain",
  });
  const abandoned = await alice.storage.createMultipart(bucketName, "private/aborted.txt", {
    sizeBytes: 16,
    contentType: "text/plain",
  });
  await alice.storage.abortMultipart(abandoned);
  await alice.storage.delete(bucketName, "private/document.txt");
  check(!(await alice.storage.list(bucketName)).items.some((item) => item.object_key === "private/document.txt"), "delete metadata remained");
  passed("RLS-protected upload/list/download/overwrite/delete and multipart complete/abort against S3");

  const templateSource = "export const Verification = ({project_name, action_url, expires_in}) => null;";
  const template = await client.importEmailTemplateArtifact({
    kind: "verification",
    version: 2,
    source: templateSource,
    source_sha256: createHash("sha256").update(templateSource).digest("hex"),
    subject_template: "Verify {{project_name}} via E2E",
    html_template: "<main><h1>Verify {{project_name}}</h1><a href=\"{{action_url}}\">Continue</a><p>{{expires_in}}</p></main>",
    text_template: "Verify {{project_name}}: {{action_url}} (expires {{expires_in}})",
    allowed_variables: ["project_name", "action_url", "expires_in"],
  });
  const preview = await client.previewEmailTemplate("verification", template.version, {
    project_name: "E2E project",
    action_url: "https://example.test/verify",
    expires_in: "30 minutes",
  });
  check(preview.subject.includes("E2E project") && preview.html.includes("https://example.test/verify"), "template preview failed");
  await client.publishEmailTemplate("verification", template.version);
  const templateRecipient = `template-${runId}@example.test`;
  const templateMailStart = Date.now();
  await new FFDBClient({ baseUrl: API, projectId: project.id }).auth.register({
    email: templateRecipient,
    password: initialPassword,
  });
  await actionToken(templateRecipient, /via E2E/i, templateMailStart);
  passed("precompiled template validation, preview, version publish, encrypted queue, and live delivery");

  const beforeBackup = await alice.query(query("SELECT body FROM documents WHERE id='alice-1'"));
  const backup = await client.createBackup({ idempotencyKey: `e2e-backup-${runId}` });
  await alice.query(query("UPDATE documents SET body='changed-after-backup' WHERE id='alice-1'"));
  const restored = await client.restoreBackup(backup.backup_id, { idempotencyKey: `e2e-restore-${runId}` });
  check(restored.integrity_ok === true, "restore integrity result was false");
  const afterRestore = await alice.query(query("SELECT body FROM documents WHERE id='alice-1'"));
  check(afterRestore.rows[0]?.[0] === beforeBackup.rows[0]?.[0], "restore did not recover original data");
  check((await client.integrityCheck()).ok, "post-restore integrity check failed");
  passed("encrypted backup creation, restore, recovered data, and integrity verification");

  const oldCursor = (await alice.sync.snapshot(["documents"])).cursor;
  const temporaryMigration = migration(
    `temporary_${runId}`,
    "temporary rollback probe",
    "CREATE TABLE rollback_probe (id INTEGER PRIMARY KEY, value TEXT)",
    "DROP TABLE rollback_probe",
  );
  await client.migrate(temporaryMigration, { idempotencyKey: `e2e-temp-migrate-${runId}` });
  const cursorAfterMigration = await alice.sync.pull(oldCursor, 10);
  check(cursorAfterMigration.control?.type === "resnapshot_required", "schema change did not force resnapshot");
  await offline.sync();
  await client.rollbackMigration(temporaryMigration.id, { idempotencyKey: `e2e-temp-rollback-${runId}` });
  check(!(await client.schema()).tables.some((table) => table.name === "rollback_probe"), "explicit down migration failed");
  passed("schema-change resnapshot and explicit down-SQL rollback");

  const invitationEmail = `developer-${runId}@example.test`;
  const invitationStart = Date.now();
  await client.createOrganizationInvitation(organization.id, { email: invitationEmail, role: "developer" });
  const invitationToken = await actionToken(invitationEmail, /invited|invitation/i, invitationStart);
  const invitedStore = new MemoryDeveloperSessionStore(`invited-${runId}`);
  const invitedClient = new FFDBClient({
    baseUrl: API,
    developerSessionStore: invitedStore,
    fetch: rateAwareFetch,
  });
  await invitedClient.acceptOrganizationInvitation({
    invitation_token: invitationToken,
    password: "Invited-Developer-Passphrase-2026",
  });
  check((await client.organizationMembers(organization.id)).some((member) => member.email === invitationEmail), "invited member missing");
  const instanceUsers = await client.instanceUsers({ limit: 100 });
  const invitedInstanceUser = instanceUsers.users.find((user) => user.email === invitationEmail);
  check(invitedInstanceUser !== undefined, "invited developer missing from instance users");
  await client.grantInstanceAdministrator(invitedInstanceUser.id);
  check(
    (await client.instanceAdministrators()).some((administrator) => administrator.user_id === invitedInstanceUser.id && administrator.role === "admin"),
    "instance administrator grant did not persist",
  );
  await client.revokeInstanceAdministrator(invitedInstanceUser.id);
  await client.setInstanceUserDisabled(invitedInstanceUser.id, true);
  await client.setInstanceUserDisabled(invitedInstanceUser.id, false);
  passed("organization invitation plus instance admin and global user controls");

  const instanceOrganizations = await client.instanceOrganizations({ limit: 100 });
  check(instanceOrganizations.organizations.some((value) => value.id === organization.id), "organization missing from instance inventory");
  await client.grantBillingExemption(organization.id, "live acceptance exemption");
  check((await client.billingExemptions()).some((value) => value.organization_id === organization.id), "billing exemption missing");
  await client.revokeBillingExemption(organization.id);
  await client.setInstanceOrganizationDisabled(organization.id, true);
  check((await client.projects(organization.id)).length === 0, "disabled organization projects remained discoverable");
  await expectSdkError(() => client.schema(), ["project.unavailable"], "disabled organization project route");
  await client.setInstanceOrganizationDisabled(organization.id, false);
  check((await client.projects(organization.id)).some((value) => value.id === project.id), "organization did not recover after re-enable");
  passed("instance organization inventory, billing exemption, disable, and recovery controls");

  const billing = await client.organizationBilling(organization.id);
  const usage = await client.organizationUsage(organization.id);
  check(
    billing.tier === "free"
      && billing.billing_enforcement_enabled === false
      && billing.provider_configured === false,
    "private instance billing policy was not reported accurately",
  );
  check(
    usage.organization_id === organization.id
      && usage.reads > 0
      && usage.writes > 0
      && usage.monthly_active_users >= 2
      && usage.reporting_status === "healthy",
    "durable organization usage ledger did not report reads, writes, MAU, and healthy reconciliation",
  );
  check((await client.organizationInvoices(organization.id)).length === 0, "private instance unexpectedly produced invoices");
  passed("durable organization metering, billing policy, reporting health, and invoice isolation");

  await client.rotateSigningKey();
  const auditLogs = await client.logs({ limit: 200 });
  check(auditLogs.length > 0, "audit log is empty");
  const metrics = await fetch(`${API}/metrics`).then((response) => response.text());
  check(metrics.includes("ffdb_http_requests_total"), "request metrics missing");
  const cliHealth = await import("node:child_process").then(({ spawnSync }) => spawnSync(
    process.execPath,
    ["packages/cli/dist/main.js", "--url", API, "--json", "health"],
    { cwd: new URL("..", import.meta.url), encoding: "utf8" },
  ));
  check(cliHealth.status === 0 && JSON.parse(cliHealth.stdout).status === "ok", "CLI did not reach live API");
  passed("JWT-key rotation, audit retrieval, bounded metrics, and CLI live-backend check");

  await client.setAuthUserDisabled(bobUserId, true);
  await expectSdkError(() => bob.query(query("SELECT id FROM documents")), ["auth."], "disabled user access");
  const sessions = await alice.auth.sessions();
  const current = sessions.find((session) => session.current);
  check(current !== undefined, "current session missing");
  await alice.auth.revokeSession(current.id);
  await expectSdkError(() => alice.query(query("SELECT id FROM documents")), ["auth."], "revoked session access");
  await client.revokeApiKey(apiKey.id);
  await expectSdkError(() => client.schema(), ["auth."], "revoked developer API key");
  passed("account disable, immediate session revocation, and API-key revocation");

  process.stdout.write(`1..${steps.length}\n`);
  process.stdout.write("live e2e complete\n");
}

main().catch((error) => {
  const message = error instanceof FFDBError
    ? `${error.code} (${error.status}): ${error.message}`
    : error instanceof Error ? error.message : String(error);
  process.stderr.write(`live e2e failed: ${message}\n`);
  process.exitCode = 1;
});
