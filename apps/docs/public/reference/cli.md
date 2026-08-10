# @ffdb/cli reference

Generated from the shipped CLI help and exported module declarations. Use --json for structured output; invalid arguments, missing credentials, API failures, and declined destructive confirmations exit non-zero.

Environment variables: `FFDB_BOOTSTRAP_TOKEN`, `FFDB_COMMERCE_STRIPE_SECRET_KEY`, `FFDB_COMMERCE_STRIPE_WEBHOOK_SECRET`, `FFDB_CONFIG`, `FFDB_DEVELOPER_KEY`, `FFDB_DEVELOPER_SESSION`, `FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY`, `FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET`, `FFDB_INSTANCE_STRIPE_SECRET_KEY`, `FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET`, `FFDB_PASSWORD`, `FFDB_PROJECT_ID`, `FFDB_URL`.

## Usage

- `ffdb [--url URL] [--project ID] [--key KEY] [--config PATH] [--json] <command>`
- `ffdb help <topic> | ffdb help all`
- `Destructive commands prompt interactively; pass --yes for automation.`

## Project setup

- `init <directory> [browser|react|node] — scaffold a starter without embedding secrets`
- `generate [output-path] — generate TypeScript types from the live schema`
- `generate --out <path> — choose the generated TypeScript output path`
- `types generate [--out <path>] — alias for generate`

## Credential lifecycle

- `login [email] — securely prompt for missing credentials; FFDB_PASSWORD supports automation`
- `logout — revoke the current session and remove its local credential`
- `project link <project-id> — persist the active project for future commands`

## Instance lifecycle

- `instance setup-status — inspect public bootstrap availability`
- `instance bootstrap [owner-email] — create the first owner using secure credentials`
- `instance status — inspect instance mode, policy, billing, and capabilities`
- `instance setup|configure <private|team> <policy> — configure a non-billing instance`
- `instance setup|configure byo <policy> — configure operator-owned Stripe credentials`
- `instance setup|configure connect <policy> <country> <email> <return-url> <refresh-url>`
- `instance policy set <owner_only|authenticated|invitation_only>`
- `instance connect onboarding <return-url> <refresh-url>`
- `instance connect refresh — refresh Stripe Connect readiness`
- `instance admins list — list delegated instance administrators`
- `instance admins grant <user-id> — grant instance administration`
- `instance admins revoke <user-id> [--yes] — revoke instance administration`
- `instance organizations [limit] [offset] — inspect organizations across the instance`
- `instance users [limit] [offset] — inspect users across the instance`
- `instance org-disable|org-enable <org-id> [--yes]`
- `instance user-disable|user-enable <user-id> [--yes]`
- `instance exemptions list — list billing exemptions`
- `instance exemptions grant <org-id> <json-file> — grant a documented exemption`
- `instance exemptions revoke <org-id> [--yes] — remove an exemption`
- `instance plans list — list instance plan definitions`
- `instance plans put <free|pay_as_you_go|pro> <json-file> — create or update a plan`
- `instance plans retire <free|pay_as_you_go|pro> [--yes] — retire a plan`
- `Bootstrap reads FFDB_BOOTSTRAP_TOKEN and FFDB_PASSWORD without printing or storing them.`
- `BYO setup reads FFDB_INSTANCE_STRIPE_SECRET_KEY and FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET.`

## Platform and project

- `org list — list organizations available to the signed-in developer`
- `org create <name> <slug> — create an organization`
- `org members <org-id> — list organization membership`
- `org invite <org-id> <email> <role> — invite an owner, admin, developer, or viewer`
- `org member-role <org-id> <user-id> <role> — update a member role`
- `org member-remove <org-id> <user-id> [--yes] — remove a member`
- `project list <org-id> — list projects in an organization`
- `project create <org-id> <name> <slug> [region] — create a project`
- `billing status <org-id> — inspect subscription status`
- `billing checkout <org-id> <pay_as_you_go|pro> — start checkout`
- `billing portal <org-id> — open subscription management`
- `billing invoices <org-id> — list invoices`
- `billing usage <org-id> — inspect billable usage`
- `commerce status — inspect the active project's commerce account`
- `commerce refresh — refresh provider capabilities`
- `commerce configure-byo — configure Stripe using secure environment variables`
- `commerce disconnect --yes — disconnect the project's commerce account`
- `commerce connect <country> <email> <return-url> <refresh-url>`
- `commerce products [--all] — list products`
- `commerce product-create <json> | commerce product-archive <id>`
- `commerce prices [--all] — list prices`
- `commerce price-create <json> | commerce price-retire <id>`
- `commerce orders | commerce payments | commerce subscriptions`
- `commerce refund <json> | commerce cancel <subscription-id> [--now]`
- `commerce portal <individual|team|organization> <subject-id> <return-url>`
- `commerce entitlements <individual|team|organization> <subject-id>`
- `commerce fulfill <order-id> <unfulfilled|processing|fulfilled|canceled> [note]`
- `api-key list — list developer API keys`
- `api-key create <name> <scope,...> — create a scoped developer key`
- `api-key revoke <id> [--yes] — revoke a developer key`

## Database workflows

- `sql <statement> — execute an inline SQL statement`
- `sql --file <path> — execute SQL from a file`
- `seed <path> — apply a seed file`
- `schema — inspect the current schema snapshot`
- `policies — inspect row-level security policies`
- `migration create <name> — create an up/down migration file`
- `migration status — inspect applied migration history`
- `migration apply <path> — apply an idempotent migration`
- `migration rollback <id> [--yes] — roll back an applied migration`

## Auth, storage, and email

- `auth settings — inspect project authentication settings`
- `auth set <json> — update authentication settings from a JSON file`
- `auth users — list project authentication users`
- `auth disable <id> [--yes] — disable an authentication user`
- `auth enable <id> — enable an authentication user`
- `storage buckets — list object-storage buckets`
- `storage create-bucket <name> [--versioning] — create a private bucket`
- `storage cleanup — clean expired upload reservations`
- `email templates — list project email templates`
- `email import-artifact <json> — import a compiled email artifact`
- `email publish <kind> <version> — publish a template version`

## Operations

- `logs [limit] — read recent project logs`
- `backup list — list project backups`
- `backup create — create a project backup`
- `backup restore <id> [--yes] — replace project data from a backup`
- `backup integrity — check project database integrity`
- `health — check API liveness`
- `dev — check liveness, readiness, API URL, and active project`

## Programmatic module exports

### FileCredentialStore

- `constructor(readonly path = defaultCredentialPath())`
- `async load(): Promise<CliCredentials | null>`
- `async save(credentials: CliCredentials): Promise<void>`
- `async clear(): Promise<void>`

- `export function parseArguments(argv: readonly string[]): ParsedArguments`
- `export function required(value: string | undefined, label: string): string`
- `export function parsePaidBillingTier(value: string | undefined): Exclude<PlatformBillingTier, "free">`
- `export function defaultCredentialPath(): string`
- `export function parseProjectTemplate(value: string | undefined): ProjectTemplate`
- `export async function scaffoldProject( targetDirectory: string, template: ProjectTemplate, options: { readonly templateRoot?: string } = {},): Promise<ScaffoldResult>`
- `export async function executeInstanceCommand( client: FFDBClient, action: string | undefined, args: readonly string[], environment: InstanceCommandEnvironment = process.env,): Promise<unknown>`
- `export function instanceConfiguration( args: readonly string[], environment: InstanceCommandEnvironment = process.env,): CompleteInstanceSetupRequest`
- `export function parseOrganizationPolicy(value: string | undefined): OrganizationCreationPolicy`
- `export function parseMigration(source: string, filename: string, createdAtMs: number): MigrationSpec`
- `export async function loadMigration(path: string, createdAtMs = Date.now()): Promise<MigrationSpec>`
- `export async function confirmDestructive( message: string, yes: boolean, io: ConfirmationIO = { input: process.stdin, output: process.stdout },): Promise<void>`
- `export function migrationIdempotencyKey(migration: MigrationSpec): string`
- `export function generateDatabaseTypes(schema: SchemaSnapshot): string`
- `export async function writeDatabaseTypes(schema: SchemaSnapshot, outputPath: string): Promise<string>`
- `export interface CliCredentials { readonly baseUrl: string; readonly projectId?: string; readonly developerKey?: string; readonly developerSessionToken?: string; readonly developerEmail?: string; readonly developerUserId?: string; readonly developerSessionExpiresAtMs?: number; }`
- `export interface ConfirmationIO { readonly input: Readable & { readonly isTTY?: boolean }; readonly output: Writable & { readonly isTTY?: boolean }; }`
- `export interface CredentialStore { load(): Promise<CliCredentials | null>; save(credentials: CliCredentials): Promise<void>; clear(): Promise<void>; }`
- `export type InstanceCommandEnvironment = Readonly<Record<string, string | undefined>>;`
- `export interface ParsedArguments { readonly options: ParsedGlobalOptions; readonly command: readonly string[]; }`
- `export interface ParsedGlobalOptions { readonly baseUrl?: string; readonly projectId?: string; readonly developerKey?: string; readonly configPath?: string; readonly json: boolean; }`
- `export type ProjectTemplate = "browser" | "node" | "react";`
- `export interface ScaffoldResult { readonly directory: string; readonly files: readonly string[]; readonly dependencies: readonly string[]; }`
