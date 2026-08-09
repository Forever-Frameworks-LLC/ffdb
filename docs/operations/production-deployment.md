# Production Deployment

FFDB runs an API/control-plane service plus isolated database workers,
PostgreSQL, S3-compatible storage, and an asynchronous email/sync worker. The API
owns durable per-organization usage ledgers below `FFDB_METRICS_ROOT`, so it is
not stateless unless a deployment provides an explicitly coordinated placement
and durable-volume design. A single host is valid for small installations;
production should place PostgreSQL and object storage on independently backed-up
services.

## Supported installation paths

The actionable single-host procedures are in the
[self-hosting installation guide](self-hosting.md). The maintained end-user path
is a versioned Compose bundle with signed, multi-architecture, digest-pinned
images; it does not require a repository checkout. The guide covers release
status, configuration, TLS handoff, persistence, verification, upgrades,
rollback, uninstall, mirrors, and the advanced native Linux bundle.

The canonical distribution is an announced tag on the
`Forever-Frameworks-LLC/ffdb` GitHub Releases page. The stable installer is the
tag selected by `releases/latest/download/install.sh`; production automation
should pin `/releases/download/vVERSION/install.sh` and pass the same exact
version. The installer verifies signed checksums and digest-pinned images before
activation. The repository's default `compose.yaml` is strictly for development
and must not expose PostgreSQL, MinIO, Mailpit, or disposable credentials
publicly. `compose.production.yaml` is the release-engineering source build
model, not an operator installation mechanism.

The bundle also includes an explicitly selected `single-host` evaluation profile
that packages PostgreSQL, MinIO, and Mailpit. It is useful for local evaluation
and starts from one installer command, but it is not an internet-production
topology: providers use local HTTP/SMTP, all durable state shares one failure
domain, and mail is captured rather than delivered. The default `external`
profile and the independently operated controls below remain recommended for
production.

## Required controls

1. Terminate TLS at a trusted proxy or directly in the service. Redirect HTTP and
   enable HSTS after validating every hostname. The shipped static gateway serves
   landing at `/`, documentation at `/docs/`, and portal at `/app/`, with SPA
   fallback confined to the docs and portal prefixes. Replace the disposable
   Compose origins in the gateway CSP `connect-src` directive with the exact HTTPS
   `FFDB_S3_PUBLIC_ENDPOINT`; do not use a wildcard or broad `https:` source.
2. Generate independent random master-encryption and cursor-HMAC keys. Store them
   in a secret manager, not environment files committed to source control.
3. Use a dedicated PostgreSQL role with TLS verification and no unrelated
   database privileges. The current release runs embedded migrations through
   `FFDB_DATABASE_URL`, so that role must retain the required control-plane schema
   migration privileges; a separate migration URL is not currently exposed.
4. Use private networking and an allowlisted HTTPS S3 endpoint. Deny public bucket
   access. Private-address server endpoints remain rejected unless
   `FFDB_S3_ALLOW_PRIVATE_NETWORK=true`; that opt-in applies only to the exact
   hostname in `FFDB_S3_ENDPOINT`, which must still use HTTPS and resolve only to
   RFC 1918 or unique-local addresses. It never relaxes validation of the
   browser-facing `FFDB_S3_PUBLIC_ENDPOINT`. Configure lifecycle cleanup only for
   explicitly documented temporary multipart prefixes.
5. Put project SQLite roots and the separate organization-metrics root on
   durable local/block storage with disk alerts. Never use an NFS-like filesystem
   unless its SQLite locking semantics are verified. Restrict the metrics root as
   billing data: it includes usage, pseudonymous MAU claims, provider-reporting
   outbox state, and reconciliation checkpoints.
6. Run database workers as a non-root user with a read-only root filesystem,
   a dedicated writable database volume, no host mounts, no extra capabilities,
   and outbound network denied except approved provider paths.
7. Configure CPU/memory/file-descriptor/pid limits and keep the API worker queue
   bounded. Scale nodes by observed active projects, not total project count.
8. Export Prometheus signals and alerts to an access-controlled system. If OTLP
   is required, install an OpenTelemetry `tracing` subscriber layer through the
   extension seam; the default binary does not ship an OTLP exporter. Redact
   authorization, cookies, provider URLs containing credentials, SQL parameters,
   template variables, and signed URLs.
9. Schedule encrypted SQLite-safe project backups plus complete coordinated host
   archives. Packaged single-host uses `ffdb-host backup`; native systemd uses
   `ffdb-backup`; external providers require a quiesced metrics snapshot paired
   with PostgreSQL and object-provider recovery points. Perform and record
   restore tests in an isolated host/project route. Backup/restore plaintext is
   staged only in the worker's mode-0700 `.ffdb-transient/<database-id>` directory;
   worker startup removes only exact UUID-named SQLite files and their WAL,
   shared-memory, or journal companions left by a crash, refuses symlinked staging
   roots, and fails an operation if its normal plaintext cleanup cannot complete.
10. Rotate API keys, JWT signing keys, master-envelope keys, PostgreSQL
    credentials, S3 credentials, and Resend keys using the runbooks.

## Upgrade order

1. Back up PostgreSQL, verify recent project backups, and capture the metrics
   ledger at a coordinated recovery point with PostgreSQL billing state.
2. Apply backward-compatible PostgreSQL migrations.
3. Deploy the API and database-worker binaries from the same release as one
   coordinated unit. Protocol version 1 is strict and does not provide dual
   decoding, so mixed API/worker releases are unsupported.
4. Deploy the unified landing/docs/portal gateway after the coordinated backend
   rollout. Preserve its subpath-aware assets, API proxy routes, security headers,
   non-root user, health check, and read-only root filesystem.
5. Drain the previous worker set only after leases expire and active transactions
   finish. A future protocol change must add dual decoding in a prior release
   before this can become a rolling mixed-version deployment.
6. Run readiness, RLS isolation, auth refresh, object-signing, sync, and restore
   smoke tests. Roll back with the documented release artifact if any gate fails.

Schema-changing project migrations are independent from platform releases. A
platform release must never silently rewrite application schema.

## Kubernetes guidance

Kubernetes is optional. Run the API and its database-worker children together in
one pod so the trusted worker binary and project volume share a lifecycle. Use a
StatefulSet when project SQLite files are node-local; attach one `ReadWriteOnce`
durable volume at `/var/lib/ffdb/projects`, a separately protected backup volume
at `/var/lib/ffdb/backups`, and a protected metrics-ledger volume at
`/var/lib/ffdb/metrics`. Do not scale that StatefulSet until the routing
and fenced project-placement procedure has assigned disjoint project databases to
pods. PostgreSQL, S3, and Resend credentials belong in a secret-store CSI driver
or equivalent, not a ConfigMap.

Set `readOnlyRootFilesystem`, `runAsNonRoot`, `allowPrivilegeEscalation: false`,
drop every capability, use a default-deny NetworkPolicy with explicit PostgreSQL
and S3/Resend egress, and set CPU/memory/ephemeral-storage/pid limits. Map
`/healthz` to liveness and `/readyz` to readiness; allow enough termination grace
for the API's shutdown/drain path. The unified static web gateway can run as a
separate stateless Deployment. Back up PostgreSQL, the encrypted project-backup
volume, and the metrics-ledger volume outside the cluster, and prove their
coordinated restore before increasing replica counts.

## ARM64 and Raspberry Pi

CI performs an ARM64 workspace check. Use the same pinned Rust toolchain and build
multi-architecture images. On low-memory hosts, reduce worker processes and the
bounded worker admission allowance, keep swap/disk latency monitored, and reserve memory for
PostgreSQL/MinIO if co-located. Do not disable authorizers, limits, or process
isolation to fit constrained hardware.
