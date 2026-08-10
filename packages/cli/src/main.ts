#!/usr/bin/env node
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

import {
  FFDBClient,
  MemoryDeveloperSessionStore,
  type AuthSettings,
  type DeveloperScope,
  type CommerceFulfillmentStatus,
  type CommerceMembershipSubjectKind,
  type CreateCommercePriceRequest,
  type CreateCommerceProductRequest,
  type CreateCommerceRefundRequest,
  type EmailTemplateArtifactInput,
  type EmailTemplateKind,
  type OrganizationRole,
} from "@ffdb/client";

import { parseArguments, required } from "./args.js";
import { parsePaidBillingTier } from "./billing.js";
import { FileCredentialStore, type CliCredentials } from "./config.js";
import { parseProjectTemplate, scaffoldProject } from "./init.js";
import { executeInstanceCommand } from "./instance.js";
import { loadMigration } from "./migration.js";
import { confirmDestructive, migrationIdempotencyKey } from "./safety.js";
import {
  collectLoginCredentials,
  loginIntro,
  renderError,
  renderHelp,
  renderHumanResult,
  supportsColor,
} from "./terminal.js";
import { writeDatabaseTypes } from "./typegen.js";

const SCOPES: readonly DeveloperScope[] = [
  "projects_read", "projects_write", "database_query", "database_migrate", "database_schema",
  "auth_manage", "storage_manage", "email_manage", "commerce_manage", "keys_rotate", "backups_manage", "logs_read",
];

async function main(argv: readonly string[]): Promise<void> {
  const parsed = parseArguments(argv);
  const [group, action, ...args] = parsed.command;
  if (group === undefined || group === "help" || group === "--help") {
    const topic = group === "help" ? action : undefined;
    process.stdout.write(renderHelp(topicHelp(topic), supportsColor(process.stdout)));
    return;
  }
  if (action === "--help" || args.includes("--help")) {
    process.stdout.write(renderHelp(topicHelp(group), supportsColor(process.stdout)));
    return;
  }

  if (group === "init") {
    const result = await scaffoldProject(
      resolve(required(action, "target directory")),
      parseProjectTemplate(args[0]),
    );
    print(result, parsed.options.json);
    return;
  }

  const store = new FileCredentialStore(parsed.options.configPath);
  const persisted = await store.load();
  const credentials = resolveCredentials(parsed.options, persisted);
  const developerSessions = new MemoryDeveloperSessionStore(`cli:${store.path}`);
  if (credentials.developerSessionToken !== undefined
    && credentials.developerEmail !== undefined
    && credentials.developerUserId !== undefined
    && credentials.developerSessionExpiresAtMs !== undefined) {
    await developerSessions.set({
      session_token: credentials.developerSessionToken,
      email: credentials.developerEmail,
      user_id: credentials.developerUserId,
      expires_at_ms: credentials.developerSessionExpiresAtMs,
    });
  }

  if (group === "login") {
    const interactive = parsed.options.json === false && process.stdin.isTTY === true && process.stderr.isTTY === true;
    if (interactive && (action === undefined || process.env.FFDB_PASSWORD === undefined)) {
      process.stderr.write(loginIntro(credentials.baseUrl, supportsColor(process.stderr)));
    }
    const { email, password } = await collectLoginCredentials(
      { ...(action === undefined ? {} : { email: action }), ...(process.env.FFDB_PASSWORD === undefined ? {} : { password: process.env.FFDB_PASSWORD }) },
      interactive,
    );
    const loginClient = new FFDBClient({
      baseUrl: credentials.baseUrl,
      developerSessionStore: developerSessions,
    });
    const session = await loginClient.developerSignIn(email, password);
    await store.save({
      baseUrl: credentials.baseUrl,
      ...(credentials.projectId === undefined ? {} : { projectId: credentials.projectId }),
      ...(credentials.developerKey === undefined ? {} : { developerKey: credentials.developerKey }),
      developerSessionToken: session.session_token,
      developerEmail: session.email,
      developerUserId: session.user_id,
      developerSessionExpiresAtMs: session.expires_at_ms,
    });
    print({ status: "authenticated", email: session.email, expires_at_ms: session.expires_at_ms, credential_path: store.path, project_id: credentials.projectId ?? null }, parsed.options.json);
    return;
  }
  if (group === "instance" && action === "bootstrap") {
    const bootstrapClient = new FFDBClient({
      baseUrl: credentials.baseUrl,
      developerSessionStore: developerSessions,
    });
    const session = await bootstrapClient.developerBootstrap(
      required(process.env.FFDB_BOOTSTRAP_TOKEN, "FFDB_BOOTSTRAP_TOKEN"),
      required(args[0], "owner email"),
      required(process.env.FFDB_PASSWORD, "FFDB_PASSWORD"),
    );
    await store.save({
      ...credentials,
      developerSessionToken: session.session_token,
      developerEmail: session.email,
      developerUserId: session.user_id,
      developerSessionExpiresAtMs: session.expires_at_ms,
    });
    print({ status: "owner_bootstrapped", user_id: session.user_id, email: session.email, expires_at_ms: session.expires_at_ms, credential_path: store.path }, parsed.options.json);
    return;
  }
  if (group === "logout") {
    const logoutClient = new FFDBClient({
      baseUrl: credentials.baseUrl,
      developerSessionStore: developerSessions,
    });
    try { await logoutClient.developerSignOut(); } catch { /* Clear the device credential even if the service is unavailable. */ }
    await store.clear();
    print({ status: "signed_out" }, parsed.options.json);
    return;
  }
  if (group === "project" && action === "link") {
    const projectId = required(args[0], "project id");
    await store.save({ ...credentials, projectId });
    print({ status: "linked", project_id: projectId, credential_path: store.path }, parsed.options.json);
    return;
  }

  const client = new FFDBClient({
    baseUrl: credentials.baseUrl,
    ...(credentials.projectId === undefined ? {} : { projectId: credentials.projectId }),
    ...(credentials.developerKey === undefined ? {} : { developerKey: credentials.developerKey }),
    developerSessionStore: developerSessions,
  });
  const result = await execute(client, parsed.command, credentials);
  print(result, parsed.options.json);
}

async function execute(client: FFDBClient, command: readonly string[], credentials: CliCredentials): Promise<unknown> {
  const [group, action, ...args] = command;
  if (group === "health") return client.health();
  if (group === "dev") return { health: await client.health(), readiness: await client.readiness(), api_url: credentials.baseUrl, project_id: credentials.projectId ?? null };
  if (group === "instance") return executeInstanceCommand(client, action, args);
  if (group === "sql") {
    const sql = action === "--file" ? await readRequiredFile(args[0]) : command.slice(1).join(" ");
    return client.query({ sql });
  }
  if (group === "seed") return client.seed(await readRequiredFile(action));
  if (group === "schema") return client.schema();
  if (group === "generate" || (group === "types" && action === "generate")) {
    const generateArguments = group === "generate" ? command.slice(1) : args;
    const outputPath = parseGenerateOutput(generateArguments);
    const schema = await client.schema();
    const path = await writeDatabaseTypes(schema, outputPath);
    return { path, schema_version: schema.version, tables: schema.tables.length };
  }
  if (group === "policies") return client.policies();
  if (group === "org" && action === "list") return client.organizations();
  if (group === "org" && action === "create") return client.createOrganization({ name: required(args[0], "organization name"), slug: required(args[1], "organization slug") });
  if (group === "org" && action === "members") return client.organizationMembers(required(args[0], "organization id"));
  if (group === "org" && action === "invite") return client.createOrganizationInvitation(required(args[0], "organization id"), { email: required(args[1], "member email"), role: parseRole(args[2]) });
  if (group === "org" && action === "member-role") return client.updateOrganizationMember(required(args[0], "organization id"), required(args[1], "user id"), { role: parseRole(args[2]) });
  if (group === "org" && action === "member-remove") { const organizationId = required(args[0], "organization id"); const userId = required(args[1], "user id"); await confirmDestructive(`Remove member ${userId} from organization ${organizationId}`, args.includes("--yes")); return client.removeOrganizationMember(organizationId, userId); }
  if (group === "project" && action === "list") return client.projects(required(args[0], "organization id"));
  if (group === "project" && action === "create") return client.createProject({ organization_id: required(args[0], "organization id"), name: required(args[1], "project name"), slug: required(args[2], "project slug"), ...(args[3] === undefined ? {} : { region: args[3] }) });
  if (group === "billing" && action === "status") return client.organizationBilling(required(args[0], "organization id"));
  if (group === "billing" && action === "checkout") return client.createBillingCheckout(required(args[0], "organization id"), { tier: parsePaidBillingTier(args[1]) });
  if (group === "billing" && action === "portal") return client.createBillingPortal(required(args[0], "organization id"));
  if (group === "billing" && action === "invoices") return client.organizationInvoices(required(args[0], "organization id"));
  if (group === "billing" && action === "usage") return client.organizationUsage(required(args[0], "organization id"));
  if ((group === "payments" || group === "commerce") && action === "status") return client.commerce.account();
  if (group === "commerce" && action === "disconnect") {
    await confirmDestructive("Disconnect this project's commerce account", args.includes("--yes"));
    await client.commerce.disconnectAccount();
    return { disconnected: true };
  }
  if (group === "commerce" && action === "configure-byo") return client.commerce.configureByo({
    secret_key: required(process.env.FFDB_COMMERCE_STRIPE_SECRET_KEY, "FFDB_COMMERCE_STRIPE_SECRET_KEY"),
    webhook_secret: required(process.env.FFDB_COMMERCE_STRIPE_WEBHOOK_SECRET, "FFDB_COMMERCE_STRIPE_WEBHOOK_SECRET"),
  });
  if (group === "commerce" && action === "connect") return client.commerce.connectOnboarding({
    country: required(args[0], "country"),
    email: required(args[1], "merchant email"),
    return_url: required(args[2], "return URL"),
    refresh_url: required(args[3], "refresh URL"),
  });
  if (group === "commerce" && action === "refresh") return client.commerce.refreshAccount();
  if (group === "commerce" && action === "products") return client.commerce.products(args.includes("--all"));
  if (group === "commerce" && action === "product-create") return client.commerce.createProduct(await readJson<CreateCommerceProductRequest>(args[0]));
  if (group === "commerce" && action === "product-archive") return client.commerce.archiveProduct(required(args[0], "product id"));
  if (group === "commerce" && action === "prices") return client.commerce.prices(args.includes("--all"));
  if (group === "commerce" && action === "price-create") return client.commerce.createPrice(await readJson<CreateCommercePriceRequest>(args[0]));
  if (group === "commerce" && action === "price-retire") return client.commerce.retirePrice(required(args[0], "price id"));
  if (group === "commerce" && action === "orders") return client.commerce.orders();
  if (group === "commerce" && action === "payments") return client.commerce.payments();
  if (group === "commerce" && action === "refund") return client.commerce.refund(await readJson<CreateCommerceRefundRequest>(args[0]));
  if (group === "commerce" && action === "subscriptions") return client.commerce.subscriptions();
  if (group === "commerce" && action === "cancel") return client.commerce.cancelSubscription(
    required(args[0], "subscription id"),
    { at_period_end: !args.includes("--now") },
  );
  if (group === "commerce" && action === "portal") return client.commerce.customerPortal({
    subject: {
      kind: required(args[0], "subject kind") as CommerceMembershipSubjectKind,
      id: required(args[1], "subject id"),
    },
    return_url: required(args[2], "return URL"),
  });
  if (group === "commerce" && action === "entitlements") return client.commerce.entitlements({
    kind: required(args[0], "subject kind") as CommerceMembershipSubjectKind,
    id: required(args[1], "subject id"),
  });
  if (group === "commerce" && action === "fulfill") return client.commerce.updateFulfillment(
    required(args[0], "order id"),
    required(args[1], "fulfillment status") as CommerceFulfillmentStatus,
    args[2] ?? null,
  );
  if (group === "api-key" && action === "list") return client.apiKeys();
  if (group === "api-key" && action === "create") return client.createApiKey({ name: required(args[0], "key name"), scopes: parseScopes(required(args[1], "comma-separated scopes")), expires_at_ms: null });
  if (group === "api-key" && action === "revoke") { const id = required(args[0], "API key id"); await confirmDestructive(`Revoke API key ${id}`, args.includes("--yes")); return client.revokeApiKey(id); }
  if (group === "migration" && action === "create") return createMigrationFile(required(args[0], "migration name"));
  if (group === "migration" && action === "status") return client.migrationHistory();
  if (group === "migration" && action === "apply") { const migration = await loadMigration(required(args[0], "migration path")); return client.migrate(migration, { idempotencyKey: migrationIdempotencyKey(migration) }); }
  if (group === "migration" && action === "rollback") { const id = required(args[0], "migration id"); await confirmDestructive(`Rollback migration ${id}`, args.includes("--yes")); return client.rollbackMigration(id); }
  if (group === "storage" && action === "buckets") return client.storage.buckets();
  if (group === "storage" && action === "create-bucket") return client.storage.createBucket({ name: required(args[0], "bucket name"), public: false, max_object_bytes: null, versioning: args.includes("--versioning") });
  if (group === "storage" && action === "cleanup") return client.storage.cleanupReservations();
  if (group === "email" && action === "templates") return client.emailTemplates();
  if (group === "email" && (action === "import-artifact" || action === "upload")) return client.importEmailTemplateArtifact(await readJson<EmailTemplateArtifactInput>(args[0]));
  if (group === "email" && action === "publish") return client.publishEmailTemplate(required(args[0], "template kind") as EmailTemplateKind, parseRequiredPositive(args[1], "template version"));
  if (group === "auth" && action === "settings") return client.authSettings();
  if (group === "auth" && action === "set") return client.updateAuthSettings(await readJson<Partial<AuthSettings>>(args[0]));
  if (group === "auth" && action === "users") return client.authUsers();
  if (group === "auth" && action === "disable") { const id = required(args[0], "auth user id"); await confirmDestructive(`Disable auth user ${id}`, args.includes("--yes")); return client.setAuthUserDisabled(id, true); }
  if (group === "auth" && action === "enable") return client.setAuthUserDisabled(required(args[0], "auth user id"), false);
  if (group === "logs") return client.logs({ limit: parsePositive(args[0], 100) });
  if (group === "backup" && action === "list") return client.backups();
  if (group === "backup" && action === "create") return client.createBackup();
  if (group === "backup" && action === "restore") { const id = required(args[0], "backup id"); await confirmDestructive(`Restore backup ${id}; current project data will be replaced`, args.includes("--yes")); return client.restoreBackup(id); }
  if (group === "backup" && action === "integrity") return client.integrityCheck();
  throw new Error(`Unknown command: ${command.join(" ")}\n\n  Run ffdb help to see common commands, or ffdb help all for the full reference.`);
}

function resolveCredentials(options: ReturnType<typeof parseArguments>["options"], persisted: CliCredentials | null): CliCredentials {
  const projectId = options.projectId ?? process.env.FFDB_PROJECT_ID ?? persisted?.projectId;
  const developerKey = options.developerKey ?? process.env.FFDB_DEVELOPER_KEY ?? persisted?.developerKey;
  const developerSessionToken = process.env.FFDB_DEVELOPER_SESSION ?? persisted?.developerSessionToken;
  return {
    baseUrl: options.baseUrl ?? process.env.FFDB_URL ?? persisted?.baseUrl ?? "http://127.0.0.1:8080",
    ...(projectId === undefined ? {} : { projectId }),
    ...(developerKey === undefined ? {} : { developerKey }),
    ...(developerSessionToken === undefined ? {} : { developerSessionToken }),
    ...(persisted?.developerEmail === undefined ? {} : { developerEmail: persisted.developerEmail }),
    ...(persisted?.developerUserId === undefined ? {} : { developerUserId: persisted.developerUserId }),
    ...(persisted?.developerSessionExpiresAtMs === undefined ? {} : { developerSessionExpiresAtMs: persisted.developerSessionExpiresAtMs }),
  };
}

function parseScopes(value: string): readonly DeveloperScope[] {
  const scopes = value.split(",").map((scope) => scope.trim()).filter(Boolean);
  for (const scope of scopes) if (!SCOPES.includes(scope as DeveloperScope)) throw new Error(`Unknown developer scope: ${scope}`);
  if (scopes.length === 0) throw new Error("At least one developer scope is required");
  return scopes as DeveloperScope[];
}

function parseRole(value: string | undefined): OrganizationRole {
  const role = required(value, "organization role");
  if (!["owner", "admin", "developer", "viewer"].includes(role)) throw new Error(`Unknown organization role: ${role}`);
  return role as OrganizationRole;
}

async function createMigrationFile(name: string): Promise<{ readonly path: string }> {
  if (!/^[a-z0-9][a-z0-9_-]{0,79}$/i.test(name)) throw new Error("migration name must contain only letters, numbers, underscore, or hyphen");
  const path = `${Date.now()}_${name}.sql`;
  await writeFile(path, `-- migrate:up\n\n-- Write forward migration SQL here.\n\n-- migrate:down\n\n-- Write explicit rollback SQL here.\n`, { encoding: "utf8", flag: "wx" });
  return { path };
}

async function readRequiredFile(path: string | undefined): Promise<string> { return readFile(required(path, "file path"), "utf8"); }
async function readJson<T>(path: string | undefined): Promise<T> { return JSON.parse(await readRequiredFile(path)) as T; }
function parsePositive(value: string | undefined, fallback: number): number { if (value === undefined) return fallback; const parsed = Number(value); if (!Number.isInteger(parsed) || parsed <= 0) throw new Error("limit must be a positive integer"); return parsed; }
function parseRequiredPositive(value: string | undefined, label: string): number { const parsed = Number(required(value, label)); if (!Number.isInteger(parsed) || parsed <= 0) throw new Error(`${label} must be a positive integer`); return parsed; }
function parseGenerateOutput(args: readonly string[]): string {
  let output: string | undefined;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === undefined) continue;
    if (argument === "--out") {
      output = required(args[index + 1], "--out path");
      index += 1;
    } else if (argument.startsWith("--out=")) {
      output = required(argument.slice("--out=".length), "--out path");
    } else if (argument.startsWith("--")) {
      throw new Error(`Unknown generate option: ${argument}`);
    } else if (output === undefined) {
      output = argument;
    } else {
      throw new Error(`Unexpected generate argument: ${argument}`);
    }
  }
  return output ?? "ffdb.types.ts";
}
function print(value: unknown, json: boolean): void {
  if (json || process.stdout.isTTY !== true) {
    process.stdout.write(typeof value === "string" ? `${value}\n` : `${JSON.stringify(value, null, 2)}\n`);
    return;
  }
  process.stdout.write(renderHumanResult(value, supportsColor(process.stdout)));
}

function help(): string {
  return `Own your projects, SQLite data, authentication, storage, and operations from one CLI.

Usage:
  ffdb <command> [options]             Run a command
  ffdb help <topic>                    Explore a command group
  ffdb help all                        Show the complete command reference

Start here:
  login [email]                        Sign in; securely prompts for email and password
  init <directory> [template]          Create a browser, React, or Node starter
  project link <project-id>            Set the active project for this machine
  dev                                  Check the API, readiness, and active project

Work with data:
  sql <statement>                      Run SQL against the active project
  schema                               Inspect the live database schema
  generate --out <path>                Generate TypeScript types from that schema
  migration <command>                  Create, apply, inspect, or roll back migrations

Explore:
  help project                         Organizations, projects, billing, commerce, and keys
  help database                        SQL, schemas, policies, seeds, and migrations
  help auth                            Authentication, storage, and email
  help instance                        Self-hosted instance setup and administration
  help operations                      Logs, backups, integrity, and health

Global options:
  --url <url>                          FFDB API URL (or FFDB_URL)
  --project <id>                       Project ID (or FFDB_PROJECT_ID)
  --key <key>                          Developer key (or FFDB_DEVELOPER_KEY)
  --config <path>                      Credential file (or FFDB_CONFIG)
  --json                               Stable machine-readable output; never prompts

Tip: Destructive commands ask for confirmation. Pass --yes only for deliberate automation.`;
}

function referenceHelp(): string {
  return `Usage:
  ffdb [--url URL] [--project ID] [--key KEY] [--config PATH] [--json] <command>
  ffdb help <topic> | ffdb help all
  Destructive commands prompt interactively; pass --yes for automation.

Project setup:
  init <directory> [browser|react|node] — scaffold a starter without embedding secrets
  generate [output-path] — generate TypeScript types from the live schema
  generate --out <path> — choose the generated TypeScript output path
  types generate [--out <path>] — alias for generate

Credential lifecycle:
  login [email] — securely prompt for missing credentials; FFDB_PASSWORD supports automation
  logout — revoke the current session and remove its local credential
  project link <project-id> — persist the active project for future commands

Instance lifecycle:
  instance setup-status — inspect public bootstrap availability
  instance bootstrap [owner-email] — create the first owner using secure credentials
  instance status — inspect instance mode, policy, billing, and capabilities
  instance setup|configure <private|team> <policy> — configure a non-billing instance
  instance setup|configure byo <policy> — configure operator-owned Stripe credentials
  instance setup|configure connect <policy> <country> <email> <return-url> <refresh-url>
  instance policy set <owner_only|authenticated|invitation_only>
  instance connect onboarding <return-url> <refresh-url>
  instance connect refresh — refresh Stripe Connect readiness
  instance admins list — list delegated instance administrators
  instance admins grant <user-id> — grant instance administration
  instance admins revoke <user-id> [--yes] — revoke instance administration
  instance organizations [limit] [offset] — inspect organizations across the instance
  instance users [limit] [offset] — inspect users across the instance
  instance org-disable|org-enable <org-id> [--yes]
  instance user-disable|user-enable <user-id> [--yes]
  instance exemptions list — list billing exemptions
  instance exemptions grant <org-id> <json-file> — grant a documented exemption
  instance exemptions revoke <org-id> [--yes] — remove an exemption
  instance plans list — list instance plan definitions
  instance plans put <free|pay_as_you_go|pro> <json-file> — create or update a plan
  instance plans retire <free|pay_as_you_go|pro> [--yes] — retire a plan
  Bootstrap reads FFDB_BOOTSTRAP_TOKEN and FFDB_PASSWORD without printing or storing them.
  BYO setup reads FFDB_INSTANCE_STRIPE_SECRET_KEY and FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET.

Platform and project:
  org list — list organizations available to the signed-in developer
  org create <name> <slug> — create an organization
  org members <org-id> — list organization membership
  org invite <org-id> <email> <role> — invite an owner, admin, developer, or viewer
  org member-role <org-id> <user-id> <role> — update a member role
  org member-remove <org-id> <user-id> [--yes] — remove a member
  project list <org-id> — list projects in an organization
  project create <org-id> <name> <slug> [region] — create a project
  billing status <org-id> — inspect subscription status
  billing checkout <org-id> <pay_as_you_go|pro> — start checkout
  billing portal <org-id> — open subscription management
  billing invoices <org-id> — list invoices
  billing usage <org-id> — inspect billable usage
  commerce status — inspect the active project's commerce account
  commerce refresh — refresh provider capabilities
  commerce configure-byo — configure Stripe using secure environment variables
  commerce disconnect --yes — disconnect the project's commerce account
  commerce connect <country> <email> <return-url> <refresh-url>
  commerce products [--all] — list products
  commerce product-create <json> | commerce product-archive <id>
  commerce prices [--all] — list prices
  commerce price-create <json> | commerce price-retire <id>
  commerce orders | commerce payments | commerce subscriptions
  commerce refund <json> | commerce cancel <subscription-id> [--now]
  commerce portal <individual|team|organization> <subject-id> <return-url>
  commerce entitlements <individual|team|organization> <subject-id>
  commerce fulfill <order-id> <unfulfilled|processing|fulfilled|canceled> [note]
  api-key list — list developer API keys
  api-key create <name> <scope,...> — create a scoped developer key
  api-key revoke <id> [--yes] — revoke a developer key

Database workflows:
  sql <statement> — execute an inline SQL statement
  sql --file <path> — execute SQL from a file
  seed <path> — apply a seed file
  schema — inspect the current schema snapshot
  policies — inspect row-level security policies
  migration create <name> — create an up/down migration file
  migration status — inspect applied migration history
  migration apply <path> — apply an idempotent migration
  migration rollback <id> [--yes] — roll back an applied migration

Auth, storage, and email:
  auth settings — inspect project authentication settings
  auth set <json> — update authentication settings from a JSON file
  auth users — list project authentication users
  auth disable <id> [--yes] — disable an authentication user
  auth enable <id> — enable an authentication user
  storage buckets — list object-storage buckets
  storage create-bucket <name> [--versioning] — create a private bucket
  storage cleanup — clean expired upload reservations
  email templates — list project email templates
  email import-artifact <json> — import a compiled email artifact
  email publish <kind> <version> — publish a template version

Operations:
  logs [limit] — read recent project logs
  backup list — list project backups
  backup create — create a project backup
  backup restore <id> [--yes] — replace project data from a backup
  backup integrity — check project database integrity
  health — check API liveness
  dev — check liveness, readiness, API URL, and active project`;
}

function topicHelp(topic: string | undefined): string {
  if (topic === undefined) return help();
  if (topic === "all") return `Complete command reference. Use ffdb help <topic> for a shorter view.\n\n${referenceHelp()}`;
  if (topic === "login" || topic === "credentials") {
    return `Sign in to FFDB without putting a password in shell history.

Usage:
  ffdb login                         Prompt securely for email and password
  ffdb login you@example.com         Prompt securely for the password
  FFDB_PASSWORD='••••' ffdb login you@example.com

Details:
  The password is never printed or saved. FFDB stores only the returned session in a mode-0600 credential file.
  Add --json for non-interactive automation; supply both the email argument and FFDB_PASSWORD.`;
  }
  const sectionNames: Readonly<Record<string, string>> = {
    setup: "Project setup", init: "Project setup", generate: "Project setup", types: "Project setup",
    instance: "Instance lifecycle",
    project: "Platform and project", org: "Platform and project", billing: "Platform and project", commerce: "Platform and project", "api-key": "Platform and project",
    database: "Database workflows", sql: "Database workflows", seed: "Database workflows", schema: "Database workflows", policies: "Database workflows", migration: "Database workflows",
    auth: "Auth, storage, and email", storage: "Auth, storage, and email", email: "Auth, storage, and email",
    operations: "Operations", logs: "Operations", backup: "Operations", health: "Operations", dev: "Operations",
  };
  const sectionName = sectionNames[topic];
  if (sectionName === undefined) throw new Error(`Unknown help topic: ${topic}\n\n  Try ffdb help, ffdb help database, or ffdb help all.`);
  const section = referenceHelp().split("\n\n").find((candidate) => candidate.startsWith(`${sectionName}:`));
  if (section === undefined) throw new Error(`Help is unavailable for ${topic}`);
  return `${section}\n\nTip: Run ffdb help all for the complete command reference.`;
}

main(process.argv.slice(2)).catch((cause: unknown) => {
  const message = cause instanceof Error ? cause.message : "Unknown CLI failure";
  if (process.argv.includes("--json")) process.stderr.write(`${JSON.stringify({ error: { message } })}\n`);
  else process.stderr.write(renderError(message, supportsColor(process.stderr)));
  process.exitCode = 1;
});
