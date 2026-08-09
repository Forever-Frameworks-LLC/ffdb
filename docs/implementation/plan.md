# Implementation and Agent Plan

## Ownership

| Owner | Primary paths | Responsibility |
| --- | --- | --- |
| Lead/integration | root manifests, `crates/protocol`, `crates/config`, `apps/api`, `docs/architecture`, CI/release | contracts, composition, integration, review, definition of done |
| SQLite/RLS specialist | `crates/sql-parser`, `crates/sqlite-rls`, `crates/sqlite-runtime`, `crates/migration-engine`, `apps/database-worker` | parser/compiler, authorizer, execution limits, workers, migrations, backups |
| Platform/security specialist | `crates/control-plane`, `crates/auth`, `crates/rate-limits`, `crates/audit`, `crates/observability`, security tests/docs | PostgreSQL schema, auth/keys/sessions, quotas, audit, continuous security review |
| Services/TypeScript specialist | `crates/object-storage`, `crates/sync-engine`, `crates/email`, `apps/sync-worker`, `packages`, `apps/portal` | storage, sync/LWW, email, SDKs, CLI, portal and UI tests |

Agents read architecture/contracts before editing, keep primary ownership
non-overlapping, and coordinate changes to shared manifests through the lead. Each
contribution includes tests and nearby documentation. The platform specialist is
the continuous security reviewer; the lead is integration/test reviewer.

## Interface freeze

Initial identifiers, protocol types, error families, worker operations, auth
context, provider traits, RLS combination semantics, migration invariants, and
package dependency direction are frozen by `docs/architecture/interfaces.md`.
Disagreements are resolved in that document and recorded in the decision log
before code diverges.

## Live release checklist

- [x] Empty repository inspected; no user changes or local instructions found.
- [x] Architecture, decisions, contracts, dependency graph, threat model, and
  ownership established.
- [x] Clean Cargo and pnpm builds pass with locked dependencies.
- [x] Docker Compose starts PostgreSQL, MinIO, API/workers, and portal with
  health checks.
- [x] Organization/project/key and exactly-one-database lifecycle works.
- [x] Migrations with custom policy syntax apply and explicit down SQL rolls back.
- [x] RLS distinguishes two users; direct backing access and end-user DDL fail.
- [x] Auth registration/verification/sign-in/refresh/reset/revoke/key rotation pass.
- [x] Storage operations and sync snapshot/pull/push/LWW/tombstones obey RLS.
- [x] Email templates compile, preview, version, queue, and deliver through the
  configured transport; production selects Resend and rejects the local SMTP
  transport.
- [x] SDK, React, React Native, CLI, and portal packages build and test; the live
  workflow exercises the SDK and CLI against the running API, and the portal was
  verified in the in-app browser.
- [x] Backups/restores, health, metrics, audit, limits, cancellation, and shutdown
  pass their live, integration, or focused regression coverage.
- [x] Unit, integration, E2E, property, bounded parser/runtime robustness, load,
  and native ARM64 gates are present and passing.
- [x] Automated safeguards and manual review evidence cover the trust boundaries,
  with no unresolved critical findings.
- [x] Operational and developer documentation includes the required runnable
  examples and passes the internal-link validator.

## Final verification evidence

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --all-features --locked`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test --workspace --all-targets --all-features --locked`
- `pnpm check`, `pnpm lint`, and `pnpm test`
- `node scripts/live-e2e.mjs`: all 15 live workflow stages passed
- `node scripts/check-doc-links.mjs`: documentation links and API/router parity
  passed
- `docker compose config --quiet`
- locked offline build and bounded corpus runs for both runtime robustness targets

## Review cadence

After each milestone the lead reviews public surfaces, runs focused tests, updates
the checklist, and checks for interface drift. Integration tests always use real
SQLite and PostgreSQL-compatible SQL migrations; mocks are limited to provider
failure injection. The final pass starts from a clean dependency install and runs
format, lint, unit, integration, E2E, security, documentation-link, and production
build checks.
