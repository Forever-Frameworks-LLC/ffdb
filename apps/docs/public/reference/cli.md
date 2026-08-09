# @ffdb/cli reference

Generated from the shipped CLI help and exported module declarations. Use --json for structured output; invalid arguments, missing credentials, API failures, and declined destructive confirmations exit non-zero.

Environment variables: `FFDB_BOOTSTRAP_TOKEN`, `FFDB_COMMERCE_STRIPE_SECRET_KEY`, `FFDB_COMMERCE_STRIPE_WEBHOOK_SECRET`, `FFDB_CONFIG`, `FFDB_DEVELOPER_KEY`, `FFDB_DEVELOPER_SESSION`, `FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY`, `FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET`, `FFDB_INSTANCE_STRIPE_SECRET_KEY`, `FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET`, `FFDB_PASSWORD`, `FFDB_PROJECT_ID`, `FFDB_URL`.

## Usage

- `ffdb [--url URL] [--project ID] [--key KEY] [--config PATH] [--json] <command>`
- `Destructive commands prompt interactively; pass --yes for automation.`

## Project setup

- `init <directory> [browser|react|node]`
- `generate [output-path] | generate --out <path> | types generate [--out <path>]`

## Credential lifecycle

- `login <email> | logout | project link <project-id>`

## Instance lifecycle

- `instance setup-status | instance bootstrap <owner-email> | instance status`
- `bootstrap reads FFDB_BOOTSTRAP_TOKEN and FFDB_PASSWORD without printing them`
- `instance setup|configure <private|team> <owner_only|authenticated|invitation_only>`
- `instance setup|configure byo <policy>  # reads FFDB_INSTANCE_STRIPE_SECRET_KEY and FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET`
- `instance setup|configure connect <policy> <country> <email> <return-url> <refresh-url>`
- `instance policy set <owner_only|authenticated|invitation_only>`
- `instance connect onboarding <return-url> <refresh-url> | instance connect refresh`
- `instance admins list | instance admins grant <user-id> | instance admins revoke <user-id> [--yes]`
- `instance organizations [limit] [offset] | instance users [limit] [offset]`
- `instance org-disable|org-enable <org-id> [--yes]`
- `instance user-disable|user-enable <user-id> [--yes]`
- `instance exemptions list | instance exemptions grant <org-id> <json-file>`
- `instance exemptions revoke <org-id> [--yes]`
- `instance plans list | instance plans put <free|pay_as_you_go|pro> <json-file>`
- `instance plans retire <free|pay_as_you_go|pro> [--yes]`

## Platform and project

- `org list | org create <name> <slug> | org members <org-id>`
- `org invite <org-id> <email> <role> | org member-role <org-id> <user-id> <role>`
- `org member-remove <org-id> <user-id> [--yes]`
- `project list <org-id> | project create <org-id> <name> <slug> [region]`
- `billing status <org-id> | billing checkout <org-id> <pay_as_you_go|pro>`
- `billing portal <org-id> | billing invoices <org-id> | billing usage <org-id>`
- `commerce status | commerce refresh | commerce configure-byo | commerce disconnect --yes`
- `commerce connect <country> <email> <return-url> <refresh-url>`
- `commerce products [--all] | commerce product-create <json> | commerce product-archive <id>`
- `commerce prices [--all] | commerce price-create <json> | commerce price-retire <id>`
- `commerce orders | commerce payments | commerce subscriptions`
- `commerce refund <json> | commerce cancel <subscription-id> [--now]`
- `commerce portal <individual|team|organization> <subject-id> <return-url>`
- `commerce entitlements <individual|team|organization> <subject-id>`
- `commerce fulfill <order-id> <unfulfilled|processing|fulfilled|canceled> [note]`
- `api-key list | api-key create <name> <scope,...> | api-key revoke <id> [--yes]`

## Database workflows

- `sql <statement> | sql --file <path> | seed <path> | schema | policies`
- `migration create <name> | migration status | migration apply <path>`
- `migration rollback <id> [--yes]`

## Auth, storage, and email

- `auth settings | auth set <json> | auth users | auth disable <id> [--yes] | auth enable <id>`
- `storage buckets | storage create-bucket <name> | storage cleanup`
- `email templates | email import-artifact <json> | email publish <kind> <version>`

## Operations

- `logs [limit] | backup list | backup create | backup restore <id> [--yes]`
- `backup integrity | health | dev`

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
