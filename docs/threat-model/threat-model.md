# FFDB Threat Model

## Scope and assets

Assets are cross-project confidentiality/integrity, application rows, identities,
credentials and signing keys, object bytes and signed URLs, logical changes and
snapshots, backups, migration history, email content, audit evidence, service
availability, and host filesystem/process integrity.

Trust boundaries: Internet-to-API, API-to-PostgreSQL, API-to-worker IPC,
worker-to-SQLite, worker-to-S3, job-to-Resend, browser-to-portal, offline replica,
backup storage, and deployment/operator access.

Actors include unauthenticated attackers, hostile end users, malicious or
compromised developers, compromised offline clients, provider/network attackers,
and operators. Developer credentials are powerful but never imply host access.

## Threats and required controls

| Threat | Primary controls | Verification |
| --- | --- | --- |
| Hostile SQL / parser discrepancy | server parse + prepare; SQLite authorizer; opcode/resource limits; isolated worker | differential, fuzz, forbidden-statement tests |
| RLS bypass/internal access | reserved namespace; physical tables/views/triggers; origin-aware authorizer; immutable context | adversarial direct/CTE/view/trigger/RETURNING/UPSERT tests |
| Pool context leak | pin transaction; RAII context; clear and verify; discard on uncertainty | cross-user contamination and cancellation tests |
| JWT forgery/routing/session confusion | verify signature/project/issuer/audience/time/kid, then require the signed session id to match a live project/user session and refresh family before route | forged/cross-project/revoked-session/reused-family tests |
| Refresh theft/reuse | random opaque rotating tokens, keyed hashes, atomic family revocation, reuse audit | concurrency and replay tests |
| API-key leakage | one-time display, scoped keys, keyed hashes, rotation/revoke, redaction | scope/constant-time/log tests |
| Filesystem/extension escape | no caller paths; canonical trusted routing; deny ATTACH/load_extension/VACUUM INTO/writable_schema/unsafe vtabs | hostile DDL and authorizer regression tests |
| Query DoS | process isolation, bounded queues/concurrency, deadline/progress handler, row/byte/memory/recursion limits | load, cancellation, recursive-query tests |
| Constraint side channels | generic errors, no protected values/names, timing budget | unique/FK/error leakage tests |
| Storage confused deputy | RLS metadata mutation first; scoped short-lived presigns; opaque keys; checksum/size quotas | cross-user and URL-scope tests |
| Signed URL leakage | minimum TTL, method/key binding, HTTPS, no logs, revocation/version strategy | presign contract tests |
| Sync leakage/stale authorization | RLS filter on pull/snapshot; scope fingerprint; policy invalidation/resnapshot | access-loss, cursor-expiry, policy-change tests |
| Cross-project worker routing | typed IDs, control-plane resolution, route generation/fencing, no paths in IPC | stale-route and project-confusion tests |
| Backup disclosure/corruption or duplicate restore | encryption, scoped access, integrity/hash, private transient staging, lifecycle fencing, and receipts copied atomically with restored SQLite state | backup/restore response-loss, intervening-write, startup-cleanup, and tamper tests |
| Secret/log leakage | secret types, field-level redaction, structured allowlisted logs, encrypted settings | log snapshot tests |
| Email injection/template RCE | isolated compilation, precompiled runtime, allowed variables, escaping, CSP preview | template and substitution tests |
| Provider SSRF | HTTPS allowlist, DNS/IP validation, redirect denial, local-dev exception | malicious endpoint tests |
| Path traversal/archive bombs | caller never controls DB path; normalized object keys; bounded streaming/decompression | traversal and oversized input tests |
| Supply chain | lockfiles, pinned CI actions/images, audit/deny policy, SBOM/signing guidance | cargo/npm audit and provenance CI |
| Abuse/rate/state exhaustion | HMAC-pseudonymous IP keys, per-IP/project/user/key buckets, exact PostgreSQL capacity counter, bounded indexed cleanup, quotas and backpressure | deterministic and concurrent saturation/recovery tests |
| Idempotency crash/replay | owner leases with heartbeats; lease-safe bounded expiry; operation-specific durable worker receipts | conflict, takeover, response-loss, and intervening-mutation tests |

## Security invariants

1. An unverified claim never selects a database, key, provider, or authorization
   context.
2. No public Rust API yields an unprotected SQLite connection.
3. End-user SQL must pass both classification and SQLite authorization.
4. Caller input is never concatenated into a filesystem path or policy SQL.
5. Internal names are inaccessible even when guessed.
6. Connection cleanup failure poisons the connection.
7. A storage or sync result cannot exceed what an equivalent RLS SELECT permits.
8. Credential plaintext is not stored; private/provider secrets are encrypted and
   never returned after creation.
9. Every state-changing privileged operation requires a successful pre-mutation
   audit append and attempts a terminal outcome append.
10. Limits fail closed and cancellation reaches worker execution.
11. A retry cannot repeat a committed migration, backup, or restore: migration
   receipts commit with SQLite history, backups reconcile the finalized encrypted
   file, and restore receipts are copied with the restored SQLite transaction.

## Residual risks and assumptions

Host administrators can access process memory and database files and must be
trusted or use external confidential-computing controls. SQLite and enabled static
extensions remain in the trusted computing base. RLS cannot retract bytes already
copied to an offline client; scope changes therefore require explicit invalidation
and the client is responsible for destroying the affected local replica. Signed
URLs remain bearer capabilities until expiry. Side-channel testing reduces but
cannot eliminate shared-host timing leakage; strict tenants should use dedicated
workers/nodes.
Generic constraint errors suppress protected names and values, but a collision
versus a successful write can still reveal the existence of an RLS-hidden primary,
unique, or foreign-key value. Schemas needing resistance to that oracle must use
opaque high-entropy, tenant-scoped keys and rate-limit attacker-controlled probes.

Security review gates occur after each security boundary lands and once more over
the complete repository. A validated security bug receives a regression test
before its fix.
