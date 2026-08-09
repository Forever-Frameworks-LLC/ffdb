# Interface Contracts

These contracts are the integration boundary. Changes require an ADR and versioned
protocol compatibility.

## Core identifiers and contexts

All IDs are UUIDv7 strings externally and typed newtypes internally. A database
route contains `(project_id, database_id, node_id, generation)` and a canonical
path resolved exclusively from trusted node configuration plus `database_id`.

```rust
pub enum ExecutionMode { Developer(DeveloperPrincipal), EndUser(AuthContext) }
pub struct AuthContext {
    pub project_id: ProjectId,
    pub subject: UserId,
    pub role: String,
    pub claims: serde_json::Map<String, serde_json::Value>,
    pub token_id: TokenId,
}
pub trait DatabaseRouter {
    async fn resolve(&self, project: ProjectId) -> Result<DatabaseRoute, PlatformError>;
}
pub trait DatabaseExecutor {
    async fn execute(&self, route: &DatabaseRoute, request: WorkerRequest)
        -> Result<WorkerExecution, PlatformError>;
}
```

`WorkerRequest` always includes a request id, route generation, execution mode,
deadline, explicit limits, schema expectation, and one typed operation. It never
contains a path. The worker rejects stale route generations.
`WorkerExecution` keeps the public `WorkerResponse` payload unchanged and adds a
server-internal, idempotent usage receipt for organization metering.

## JSON SQL protocol

```json
{
  "sql": "select id, payload from documents where id = ?1",
  "parameters": [{"type":"integer","value":42}],
  "options": {"max_rows":1000}
}
```

Parameter types are `null`, `integer` (decimal string accepted), `real`, `text`,
and `blob` (`base64`). Results preserve column and row order:

```json
{
  "columns": [{"name":"id","type":"integer"},{"name":"payload","type":"blob"}],
  "rows": [[42,{"$blob":"AQI="}]],
  "affected_rows": 0,
  "last_insert_rowid": null,
  "truncated": false
}
```

Integers outside `[-9007199254740991, 9007199254740991]` are decimal strings.
Duplicate names remain distinct column entries. Date/time values are ordinary
integer Unix epochs; declared-type metadata identifies their interpretation.

Transactions contain 1..100 query objects, execute on one pinned connection, and
either return all results or none. Server-owned transaction control is not valid
inside statement SQL.

## Errors

```json
{"error":{"code":"query.statement_not_allowed","message":"statement is not allowed","request_id":"...","details":{"kind":"pragma"}}}
```

Codes are stable; messages are safe and may evolve. Details are allowlisted.
Minimum families: `auth.*`, `api_key.*`, `project.*`, `query.*`, `migration.*`,
`rls.*`, `storage.*`, `sync.*`, `quota.*`, `rate_limit.*`, `provider.*`, and
`internal.*`. Constraint errors do not reveal protected row values or internal
object names.

## HTTP authentication

- Developer routes: `Authorization: Bearer ffdb_dev_<prefix>.<secret>` with scope
  checks. Only the prefix is used for lookup; the secret is constant-time verified.
- End-user routes: `Authorization: Bearer <JWT>`. `kid` selects an active or
  grace-period public key for the path project, before routing is trusted.
- Refresh cookies/tokens are accepted only on auth refresh/revoke routes and are
  rotated atomically.

State-changing requests require `Idempotency-Key` where documented. Every
response includes a server-generated `X-Request-Id`. Internal integrations that
adopt an external correlation id must parse it with the strict observability
format before recording it.

## Migrations and RLS compiler

```rust
pub struct MigrationSpec { pub id: String, pub name: String, pub up_sql: String,
    pub down_sql: String, pub checksum: Sha256Digest, pub created_at_ms: i64 }
pub trait RlsCompiler {
    fn parse(&self, sql: &str) -> Result<Vec<RlsStatement>, RlsError>;
    fn compile(&self, schema: &SchemaSnapshot, statements: &[RlsStatement])
        -> Result<CompiledRlsPlan, RlsError>;
}
```

Migration application acquires the lifecycle lock, verifies id/checksum, drains
active sessions, runs SQL and metadata changes atomically, increments schema
version, appends a scope-invalidation change, and invalidates worker statements.
Rollback executes only the stored developer-supplied `down_sql`.

## Auth, email, storage, and sync adapters

```rust
pub trait PasswordHasher { fn hash(&self, password: SecretString) -> Result<PasswordHash>; fn verify(&self, password: SecretString, hash: &PasswordHash) -> Result<VerifyOutcome>; }
pub trait SigningKeyStore { async fn active_signer(&self, project: ProjectId) -> Result<Signer>; async fn verification_keys(&self, project: ProjectId) -> Result<Vec<VerificationKey>>; }
pub trait EmailTransport { async fn send(&self, message: PrecompiledMessage) -> Result<ProviderMessageId>; }
pub trait ObjectStore { async fn presign(&self, operation: AuthorizedObjectOperation, ttl: Duration) -> Result<SignedObjectRequest>; }
```

Email substitution accepts only declared scalar variables and escapes HTML by
default. Storage adapters receive an already authorized opaque provider key, never
an arbitrary provider URL. Sync cursor payloads are authenticated and opaque;
clients must treat them as uninterpreted strings.

## Package compatibility

The API publishes OpenAPI plus a protocol schema. `@ffdb/client` is the only
package that speaks HTTP. React, React Native, sync, and CLI packages depend on
client types and public methods, not portal internals or generated server code.

## Billing domain separation

`PlatformBillingProvider` creates organization-scoped Checkout and Customer
Portal sessions and verifies provider webhooks. `ProjectCommerceProvider` is a
separate extension boundary keyed by `ProjectId` and a project-commerce account.
Neither interface accepts the other domain's customer, subscription, or account
identifier. Project commerce models charge behavior as `destination` or `direct`
plus explicit responsibility/capability configuration, never a legacy bundled
Connect account-type label.
