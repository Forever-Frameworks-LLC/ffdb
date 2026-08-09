# Local Development

## Prerequisites

- Rust 1.96.1 with rustfmt and Clippy
- Node.js 24 or newer and pnpm 11.6.0
- Docker Engine/desktop with Compose v2
- optional PostgreSQL client and `curl`

Run the repository bootstrap once. It checks the required tools, creates `.env`
from `.env.example` only when missing, installs locked dependencies, fetches the
locked Cargo graph, and validates the Compose model:

```bash
make bootstrap
```

Values in `.env.example` are disposable local credentials and must never be
reused in production. Bootstrap never overwrites an existing `.env`.

For an operator deployment, stop here and use the
[self-hosting installation guide](self-hosting.md). The default Compose file is a
development fixture; the production source-build model is
`compose.production.yaml` and intentionally requires external durable providers.

## Full local stack

```bash
make compose-rebuild
make status
```

`compose-rebuild` is the canonical source-to-container path. Compose builds the
API and sync worker through `infra/docker/Dockerfile.rust` and the unified web
gateway through `infra/docker/Dockerfile.portal`; both build stages copy the
current checkout. The gateway Dockerfile produces subpath-aware production
builds for landing, docs, and portal and installs them into one nginx image.
The command uses `--build`, `--force-recreate`, and `--wait`, so already-running
services cannot silently remain on an older image. Recreating containers does
not remove their named volumes.

PostgreSQL listens on `5432`, MinIO on `9000` (console `9001`), and Mailpit on
`1025` (UI `8025`). The API is at `8080`. The static gateway is at `5173`, with
landing at `/`, documentation at `/docs/`, and the portal at `/app/`. The API
applies every versioned platform migration under `infra/postgres/migrations`
before it begins listening. `minio-bootstrap` creates a private provider bucket;
it exits successfully rather than remaining as a daemon.

Compose publishes each development port on `127.0.0.1` only. Use a reviewed
reverse proxy and TLS configuration instead of broadening those bindings on a
shared machine.

The API container launches bounded database-worker child processes against the
project-data volume. Per-organization usage, storage, and MAU accounting is
stored in the separate metrics-data volume. `sync-worker` is a maintenance
process for durable sync checkpoint artifacts; RLS-authorized snapshot/pull/push
execution remains in the database worker so there is no second authorization
path.

For infrastructure-only development use `make infra-up`, then run the Rust API
and any Vite app from the host. The standalone development URLs remain
`127.0.0.1:5173` for portal, `127.0.0.1:5174` for landing, and
`127.0.0.1:5175` for docs; standalone builds/previews retain root-relative asset
paths. Only the gateway production builds use `/app/` and `/docs/` asset bases.
`make dev-up` remains an alias for the canonical Compose rebuild, and
`make dev-down` stops containers without deleting volumes. Portal Vite and the
gateway proxy `/v1`, `/healthz`, `/readyz`, and `/openapi.json` to the API. Raw
`/metrics` stays on the direct development API listener.

## Build and test

```bash
make build
make check
make test
```

`make check` includes Rust formatting and Clippy, TypeScript build/type checks,
documentation/API-contract checks, and Compose validation. `make verify` runs
check, test, and build in the same command surface used locally.

Run a single Rust component with `cargo test -p <package>`. Run a TypeScript
package with `pnpm --filter <package> test`.

## Runnable full-stack example

With Compose healthy and the TypeScript packages built, run:

```bash
make live
```

The live target builds the host TypeScript distributions and all three Vite
applications, rebuilds and waits for the current-source Compose stack and web
gateway, and only then runs `pnpm test:live`. Invoke `pnpm test:live` directly
only when intentionally testing an already-managed remote or existing stack.

The harness at `scripts/live-e2e.mjs` uses only public HTTP, SDK, provider, and
Mailpit surfaces. It creates a unique organization and project, applies explicit
up/down SQL containing `CREATE TABLE`, `ENABLE/FORCE ROW LEVEL SECURITY`, and
`CREATE POLICY`, registers and verifies two users, and proves their identical
queries return different rows. It also exercises protected object upload,
download, overwrite, multipart completion/abort and delete; offline snapshot,
push, pull, replay, server-sequence conflict resolution, tombstones, and forced
resnapshot; template artifact import/preview/publish/delivery; API-key and session
revocation; and encrypted backup/restore.

On an already-bootstrapped installation, provide the existing local developer
credentials without putting them in command history:

```bash
FFDB_E2E_DEVELOPER_EMAIL=admin@example.test \
FFDB_E2E_DEVELOPER_PASSWORD="$FFDB_TEST_PASSWORD" pnpm test:live
```

The default credentials are disposable local values only. The harness never
prints a developer key, session token, end-user token, signed URL, or email
action token.

## Data safety

Project databases live below the configured relative `FFDB_DATABASE_ROOT`, and
organization usage ledgers live below `FFDB_METRICS_ROOT`. The API accepts
project identifiers, never paths. `make clean` runs a volume-retaining
Compose shutdown and removes only Cargo/TypeScript/fuzz build outputs. It does not
remove `data/`, `node_modules`, the pnpm store, or any named Docker volume.
`docker compose down` likewise retains volumes. Adding `--volumes` is deliberately
absent from every repository command because it irreversibly removes this
Compose project's local provider and project state.

## Command reference

| Command | Purpose | Persistent-data effect |
| --- | --- | --- |
| `make bootstrap` | Install/fetch locked dependencies and validate Compose | Creates `.env` only if absent |
| `make build` | Build all Rust and TypeScript targets | None |
| `make check` | Run static, docs, API-contract, and Compose checks | None |
| `make test` | Run Rust and TypeScript test suites | Test-owned temporary state only |
| `make live` | Rebuild current sources and run public live E2E | Adds isolated E2E records to local volumes |
| `make compose-rebuild` | Rebuild, recreate, and wait for the complete stack | Retains all named volumes |
| `make status` | Show containers and their image identities | None |
| `make clean` | Stop containers and remove reproducible build outputs | Retains all application/provider data |

## Health and diagnostics

```bash
curl --fail http://localhost:8080/healthz
curl --fail http://localhost:8080/readyz
curl --fail http://localhost:8080/openapi.json
curl --fail http://localhost:8080/metrics
```

Every API response includes `X-Request-Id`. Use it to correlate structured logs,
audit events, and traces. Never paste bearer credentials into issue reports.

## Email, browser credentials, and storage cleanup

Compose uses local SMTP delivery to Mailpit. Production configuration rejects
SMTP and requires `FFDB_EMAIL_TRANSPORT=resend` plus a real
`FFDB_RESEND_API_KEY`. React Email source is compiled by a trusted developer
tool into the bounded artifact format before upload; the API and delivery worker
never execute JavaScript or JSX.

The portal keeps its short-lived developer session and the selected project's API
key in `sessionStorage`. This limits persistence across browser restarts but does
not defeat same-origin XSS, so keep the shipped CSP, never inject untrusted HTML,
and never put a credential in a `VITE_*` build variable. Newly issued key
plaintext is shown only at initial creation.

Signed S3 operations use `FFDB_S3_PUBLIC_ENDPOINT` for the browser-visible URL
and `FFDB_S3_ENDPOINT` for server-side verification. In production both must be
HTTPS and object storage must enforce an exact portal-origin CORS allowlist.

Upload reservations are durable and expire safely, but cleanup is deliberately
an operator action. Schedule an authenticated
`POST /v1/projects/{project_id}/storage/cleanup` with a `storage_manage` API key
at an interval shorter than the provider's abandoned-object retention policy.
The endpoint claims a bounded batch of expired work, deletes abandoned regular
objects or aborts abandoned multipart uploads using server-held provider keys,
and acknowledges only successful provider operations. Failed operations stay in
the durable queue for retry. Its public response contains only `removed` and
`retried` counts; it never accepts or returns a provider key, upload id, path, or
prefix. The endpoint is safe to schedule repeatedly.
