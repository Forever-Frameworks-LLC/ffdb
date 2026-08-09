# HTTP API Reference

The stable base path is `/v1`. Requests and responses use JSON unless an endpoint
returns an S3-compatible signed operation. Every response includes
`X-Request-Id`. The API generates a fresh UUIDv7 for every inbound request and
does not trust a client-supplied request identifier.

## Credentials

Organization management uses the short-lived platform developer session returned
by `/v1/developer/sign-in`. Project operations use
`Authorization: Bearer ffdb_dev_<prefix>.<secret>` and require the documented
scope. End-user operations use a project-issued JWT.
Refresh tokens are accepted only by refresh and revocation endpoints. Tokens,
signed URLs, and secrets must not be put in query strings or logs.

The server verifies the credential for the path project before resolving its
database route. A credential for another project returns the same safe denial as
an unauthorized resource.

## First-run instance ownership

`GET /v1/instance/setup/status` is the only public instance-discovery route. It
returns four booleans—`bootstrap_available`, `setup_required`,
`platform_byo_available`, and `platform_connect_available`—and never exposes an
owner identity, provider account, or credential state. A fresh host accepts the
installation's one-time bootstrap token at `POST /v1/developer/bootstrap`; the
created platform user and immutable instance-owner record commit together.
Later bootstrap attempts return a conflict.

After bootstrap, the owner completes `POST /v1/instance` with an
`Idempotency-Key` and one tagged deployment request:

```json
{
  "deployment_mode": "private",
  "organization_creation_policy": "owner_only"
}
```

`deployment_mode` may be `private`, `team`, `platform_byo`, or
`platform_connect`. BYO additionally accepts write-only `secret_key` and
`webhook_secret` values and provisions the FFDB billing catalog in that Stripe
account. Connect also requires the operator's write-only Stripe Connect platform
`secret_key` and Connect `webhook_secret`, plus `country`, `email`, HTTPS
`return_url`, and HTTPS `refresh_url`. It creates or resumes Accounts v2
onboarding and provisions the account-scoped catalog only after Stripe reports
the merchant ready. Provider failure leaves billing inactive and retryable; it
never silently commits a partly validated catalog. Neither mode returns the
supplied credentials.

| Method | Route | Authorization and purpose |
| --- | --- | --- |
| `GET` | `/instance/setup/status` | public, non-secret first-run discovery |
| `GET/POST` | `/instance` | current status; owner-only setup/reconfiguration for `POST` |
| `PATCH` | `/instance/organization-creation-policy` | instance administrator |
| `POST` | `/instance/billing/connect/onboarding` | owner; idempotency key |
| `POST` | `/instance/billing/refresh` | owner; refresh readiness and provision/validate catalog |
| `GET/POST` | `/instance/administrators` | instance administrator; only an existing platform user can be granted |
| `DELETE` | `/instance/administrators/{user_id}` | instance administrator; owner cannot be removed |
| `GET` | `/instance/organizations`, `/instance/users` | paginated global administration |
| `PATCH` | `/instance/organizations/{organization_id}` | disable/re-enable an organization without deleting its durable state |
| `PATCH` | `/instance/users/{user_id}` | disable/re-enable a platform user; the immutable owner is protected |
| `GET` | `/instance/billing-exemptions` | list organizations exempt from charges but still measured for analytics |
| `PUT/DELETE` | `/instance/billing-exemptions/{organization_id}` | grant with a reason or revoke an exemption |
| `GET` | `/instance/plans` | Free, pay-as-you-go, and Pro catalog |
| `PUT/DELETE` | `/instance/plans/{tier}` | owner-controlled plan policy; active subscriptions prevent unsafe retirement |

Private and team modes clear instance billing secrets/catalog bindings and
disable tenant charging while retaining the per-organization usage ledger.
Only the owner may change deployment mode or provider credentials; delegated
instance admins can operate users, organizations, plan policy, and exemptions.

## Route summary

### Platform and projects

| Method | Route | Credential / scope |
| --- | --- | --- |
| `POST` | `/developer/bootstrap` | one-time bootstrap token |
| `POST` | `/developer/sign-in`, `/developer/refresh`, `/developer/sign-out` | password/session token |
| `POST` | `/developer/invitations/accept` | invitation token and new password |
| `GET/POST` | `/organizations` | platform developer session |
| `GET` | `/organizations/{organization_id}/projects` | organization member |
| `GET/POST` | `/organizations/{organization_id}/members` | organization owner/admin |
| `PATCH/DELETE` | `/organizations/{organization_id}/members/{user_id}` | organization owner/admin |
| `POST` | `/organizations/{organization_id}/invitations` | organization owner/admin |
| `POST` | `/projects` | organization owner/admin; idempotency key |
| `GET/POST` | `/projects/{project_id}/api-keys` | owner/admin |
| `POST` | `/projects/{project_id}/api-keys/{key_id}/revoke` | owner/admin |
| `POST` | `/projects/{project_id}/keys/rotate` | `keys_rotate` |

API key plaintext is returned exactly once from creation. Project creation
provisions exactly one database. `Idempotency-Key` is required for project
creation, migration apply/rollback, backup creation, and restore. Its durable
request hash rejects key reuse with different content, concurrent ownership uses
a heartbeat-renewed bounded lease, expired/completed rows are purged in bounded
lease-safe batches, and successful responses can be replayed. Durable worker
receipts reconcile response loss for migrations, backup creation, and restore;
server/credential failures are never cached.

### Organization billing

Platform billing is owned by the FFDB deployment and is separate from anything
a customer project sells. Private/team instances return analytics without
charges. A monetized BYO/Connect instance enforces the active catalog and makes
provider redirects available to organization owners/admins.

| Method | Route | Credential / behavior |
| --- | --- | --- |
| `GET` | `/organizations/{organization_id}/billing` | organization member; effective tier, limits, enforcement, and provider readiness |
| `GET` | `/organizations/{organization_id}/billing/usage` | organization member; reads, writes, storage byte-hours, MAU, period, and reporting health |
| `GET` | `/organizations/{organization_id}/billing/invoices` | organization member; verified durable invoice history |
| `POST` | `/organizations/{organization_id}/billing/checkout` | owner/admin; `pay_as_you_go` or `pro`; idempotency key |
| `POST` | `/organizations/{organization_id}/billing/portal` | owner/admin; idempotency key |
| `POST` | `/billing/webhooks/stripe` | raw body plus valid `Stripe-Signature`; no bearer credential |

Free includes two projects, 1 GB storage, 1 million monthly reads, 50,000
monthly writes, and 5,000 MAU. PAYG includes the same allowance, then bills
$0.20/GB-month from byte-hours, $0.25/million reads, $1.50/million writes
through one million and $2.25/million afterward, and $0.005/MAU through 50,000
then $0.015. Pro is $7/month and includes 10 GB, 15 million reads, 750,000
writes, and 50,000 MAU before the same usage dimensions apply. On Free, reads
continue at the allowance while write, new-MAU, and storage-growth admission
pause. Exempt organizations bypass charges/admission but retain analytics.

Successful worker operations write idempotent usage receipts to the private
per-organization SQLite ledger. Its durable outbox reports only positive deltas;
reconciliation and verified webhooks—not browser redirects—are authoritative
for paid state and invoices.

### Project commerce

Project commerce is optional per project. A project can remain unconfigured,
store encrypted project-owned Stripe credentials, or use a Connect account with
direct charges. The selected merchant account owns its Products, Prices,
Customers, payments, refunds, and subscriptions; platform-billing credentials
are never reused.

| Method | Route | Credential / behavior |
| --- | --- | --- |
| `GET` | `/projects/{project_id}/payments` | compatibility capability summary |
| `GET` | `/projects/{project_id}/commerce/account` | project owner/admin or `commerce_manage`; credentials are redacted |
| `DELETE` | `/projects/{project_id}/commerce/account` | audited, idempotent local disconnect for unused BYO/Connect bindings; returns `commerce.account_in_use` after provider-bound state exists and never closes the Stripe account |
| `POST` | `/projects/{project_id}/commerce/account/byo` | write-only key configuration/rotation; idempotency key |
| `POST` | `/projects/{project_id}/commerce/account/connect/onboarding` | Accounts v2 onboarding; idempotency key |
| `POST` | `/projects/{project_id}/commerce/account/refresh` | refresh provider readiness |
| `GET/POST` | `/projects/{project_id}/commerce/products` | public active catalog read; managed create |
| `DELETE` | `/projects/{project_id}/commerce/products/{product_id}` | archive; idempotency key |
| `GET/POST` | `/projects/{project_id}/commerce/prices` | public active catalog read; managed immutable-price create |
| `DELETE` | `/projects/{project_id}/commerce/prices/{price_id}` | retire; idempotency key |
| `POST` | `/projects/{project_id}/commerce/checkouts/one-time` | end-user or `commerce_manage`; idempotency key |
| `POST` | `/projects/{project_id}/commerce/checkouts/recurring` | subject-bound membership Checkout; idempotency key |
| `POST` | `/projects/{project_id}/commerce/customer-portal` | subject-authorized Customer Portal; idempotency key |
| `GET` | `/projects/{project_id}/commerce/orders`, `/commerce/payments`, `/commerce/subscriptions` | project commerce management |
| `PATCH` | `/projects/{project_id}/commerce/orders/{order_id}/fulfillment` | paid-state-gated fulfillment; idempotency key |
| `POST` | `/projects/{project_id}/commerce/refunds` | bounded provider refund; idempotency key |
| `POST` | `/projects/{project_id}/commerce/subscriptions/{subscription_id}/cancel` | immediate or period-end cancellation; idempotency key |
| `GET` | `/projects/{project_id}/commerce/entitlements` | authenticated, subject-scoped entitlement read |
| `POST` | `/projects/{project_id}/commerce/webhooks/stripe` | BYO only: exact raw body and the project's endpoint signature; Connect events are rejected |
| `POST` | `/commerce/webhooks/stripe-connect` | Connect only: global endpoint-secret verification happens before `event.account` is parsed and routed to exactly one connected project |

Checkout redirects never grant an entitlement or advance fulfillment. Verified,
ordered, idempotent webhook events create the durable payment/subscription state;
captured funds must cover an order before fulfillment can advance. A credential
rotation may preserve the same Stripe account, while rebinding a project with
provider-bound state to another account is rejected.

### SQL and migrations

| Method | Route | Credential / scope |
| --- | --- | --- |
| `POST` | `/projects/{project_id}/query` | project JWT or `database_query` |
| `POST` | `/projects/{project_id}/transaction` | project JWT or `database_query` |
| `GET` | `/projects/{project_id}/migrations?limit=` | `database_migrate` |
| `POST` | `/projects/{project_id}/migrations` | `database_migrate`; idempotency key |
| `POST` | `/projects/{project_id}/migrations/{id}/rollback` | `database_migrate`; idempotency key |
| `POST` | `/projects/{project_id}/seed` | `database_query` |
| `GET` | `/projects/{project_id}/schema` | `database_schema` |
| `GET` | `/projects/{project_id}/policies` | `database_schema` |

Query requests contain one statement and typed parameters:

```json
{
  "sql": "select id, payload from documents where owner_id = ?1",
  "parameters": [{"type":"text","value":"usr_123"}],
  "options": {"max_rows":1000}
}
```

Transactions contain `{"statements":[<query>, ...]}` with 1–100 statements.
The server pins one worker connection and owns transaction control. End-user SQL
is limited to `SELECT`, `INSERT`, `UPDATE`, and `DELETE` by parser classification
and the SQLite authorizer.

Results preserve duplicate columns safely:

```json
{
  "columns": [
    {"name":"id","type":"integer"},
    {"name":"payload","type":"blob"}
  ],
  "rows": [["9223372036854775807", {"$blob":"AQI="}]],
  "affected_rows": 0,
  "last_insert_rowid": null,
  "truncated": false
}
```

Integers outside JavaScript's exact range are decimal strings; BLOBs are tagged
base64 objects; NULL is JSON null. Date/timestamp declared types use Unix epoch
milliseconds.

A migration contains stable `id`, `name`, `up_sql`, developer-supplied
`down_sql`, `checksum`, and `created_at_ms`. The SHA-256 checksum covers id, name,
both SQL directions, and separators. Reusing an id with different contents is a
conflict. Rollback never attempts inferred inverse DDL.

### End-user authentication

| Method | Route | Credential |
| --- | --- | --- |
| `POST` | `/projects/{project_id}/auth/register` | anonymous, rate limited |
| `POST` | `/projects/{project_id}/auth/verify` | verification token |
| `POST` | `/projects/{project_id}/auth/sign-in` | anonymous, rate limited |
| `POST` | `/projects/{project_id}/auth/refresh` | refresh token |
| `POST` | `/projects/{project_id}/auth/sign-out` | refresh token |
| `POST` | `/projects/{project_id}/auth/password/reset` | anonymous, enumeration safe |
| `POST` | `/projects/{project_id}/auth/password/reset/complete` | reset token |
| `POST` | `/projects/{project_id}/auth/password/change` | access token |
| `GET` | `/projects/{project_id}/auth/sessions` | access token |
| `DELETE` | `/projects/{project_id}/auth/sessions/{session_id}` | access token |
| `GET/PATCH` | `/projects/{project_id}/auth/settings` | API key with `auth_manage` |
| `GET` | `/projects/{project_id}/auth/users` | API key with `auth_manage` |
| `PATCH` | `/projects/{project_id}/auth/users/{user_id}` | API key with `auth_manage` |

Registration and reset initiation return a generic accepted result where needed
to prevent identity enumeration. A successful sign-in/refresh returns an access
token, rotating refresh token, expiry, session id, and safe user record. Reusing a
rotated refresh token revokes its family and creates an audit event.

### Logical offline synchronization

| Method | Route | Credential |
| --- | --- | --- |
| `GET` | `/projects/{project_id}/snapshot` | project JWT |
| `GET` | `/projects/{project_id}/sync?cursor=&limit=` | project JWT |
| `POST` | `/projects/{project_id}/sync/push` | project JWT |

Cursors are opaque authenticated strings. A pull returns ordered logical changes,
the next cursor, `has_more`, and an optional control event. Clients must stop and
resnapshot on `resnapshot_required`. Push accepts at most 100 mutations with
unique client mutation IDs and returns an independent result for each. Server
sequence establishes last-write-wins; client time is retained as metadata only.

### Object storage

| Method | Route | Credential |
| --- | --- | --- |
| `GET` | `/projects/{project_id}/storage/buckets` | `storage_manage` |
| `POST` | `/projects/{project_id}/storage/buckets` | `storage_manage` |
| `GET` | `/projects/{project_id}/storage/objects` | project JWT |
| `POST` | `/projects/{project_id}/storage/sign` | project JWT |
| `POST` | `/projects/{project_id}/storage/commit` | project JWT |
| `POST` | `/projects/{project_id}/storage/release` | project JWT |
| `POST` | `/projects/{project_id}/storage/multipart/authorize` | project JWT |
| `POST` | `/projects/{project_id}/storage/multipart/create` | project JWT |
| `POST` | `/projects/{project_id}/storage/multipart/commit` | project JWT |
| `POST` | `/projects/{project_id}/storage/cleanup` | `storage_manage` |

All operations first execute an RLS-protected metadata read or mutation on the
project SQLite database. Signed URLs are method/key/version bound, short lived,
and never returned for an unauthorized metadata operation. Upload, download,
delete, part-upload, complete, and abort are expressed as a bounded `operation`
in `/storage/sign`; multipart create is separately authorized with an exact total
size and then proxied through `/storage/multipart/create`, so the client cannot
substitute a provider upload id. Multipart parts remain direct signed S3
requests. Metadata commits are replay-safe and occur only after provider success
or verification. A provider-successful write is retained for durable cleanup if
the metadata commit cannot be confirmed; provider failures release the exact
nonce/byte/expiry reservation. Schedule `/storage/cleanup` as an operator job;
each call processes a bounded batch and reports only `removed` and `retried`
counts. The API does not pretend a manual endpoint is an automatic scheduler.
Bucket versioning is rejected until an explicitly configured provider
implementation supports it.

### Developer configuration and operations

| Method | Route | Scope |
| --- | --- | --- |
| `GET/PATCH` | `/projects/{project_id}/auth/settings` | `auth_manage` |
| `GET` | `/projects/{project_id}/email/templates?kind=` | `email_manage` |
| `POST` | `/projects/{project_id}/email/templates/artifacts` | `email_manage` |
| `POST` | `/projects/{project_id}/email/templates/{kind}/{version}/preview` | `email_manage` |
| `POST` | `/projects/{project_id}/email/templates/{kind}/{version}/publish` | `email_manage` |
| `GET` | `/projects/{project_id}/logs` | `logs_read` |
| `GET` | `/projects/{project_id}/backups?limit=` | `backups_manage` |
| `POST` | `/projects/{project_id}/backups` | `backups_manage`; idempotency key |
| `POST` | `/projects/{project_id}/backups/{id}/restore` | `backups_manage`; idempotency key |
| `POST` | `/projects/{project_id}/integrity-check` | `backups_manage` |

React Email/JSX compilation is a trusted developer-tool operation outside the
request-serving API. The artifact upload recomputes the source digest and
revalidates bounded HTML/text, allowed variables, URL schemes, and unsafe markup.
The API, outbox, and delivery worker never execute uploaded JavaScript. Preview
and request-time delivery perform only allowlisted variable substitution into a
validated artifact. Provider selection is deployment configuration: local
development uses Mailpit SMTP and production requires Resend.

## Errors and retries

```json
{
  "error": {
    "code": "query.statement_not_allowed",
    "message": "statement is not allowed",
    "request_id": "0194f6a0-6450-7c24-a236-0ab8b6f4ee2b",
    "details": {"kind":"pragma"}
  }
}
```

Codes are stable; text is not. Safe details are endpoint-specific. Internal
names, paths, SQL fragments, credential state, and protected values are omitted.

- `400`: malformed or unsupported request; do not retry unchanged.
- `401`: missing/invalid/expired credential; refresh once when applicable.
- `403`: authenticated but disallowed; do not retry unchanged.
- `404`: absent or deliberately concealed resource.
- `409`: migration/checksum/idempotency/schema conflict; reconcile state.
- `413`: request or response-size limit.
- `429`: rate/concurrency/queue limit; honor `Retry-After` with jitter.
- `503`: project/worker/provider temporarily unavailable; retry only idempotent
  operations or requests carrying a stable idempotency key.
- `504`: server execution deadline; outcome of an unkeyed mutation may be unknown.

SDK retries are bounded, jittered, cancellation-aware, and never automatically
repeat a non-idempotent mutation without an idempotency key.
