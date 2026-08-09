# Local load testing

FFDB includes a bounded, dependency-free HTTP load smoke for comparing the
compiled nginx gateway, direct Axum handling, and PostgreSQL-backed readiness.
It is a regression and diagnostics tool, not a published capacity claim.
Results depend on the host, container runtime, logging level, storage, and
PostgreSQL configuration.

## Safe default run

Start the current Compose build, confirm it is healthy, and run all three local
profiles:

```bash
make compose-rebuild
make load-test
```

The aggregate target runs 300 measured requests per profile, 12 concurrent
requests, 12 warmups, and a two-second per-request timeout. Every target is a
loopback-only `GET`; it does not create accounts, projects, database rows,
objects, billing state, or email. The harness refuses remote hosts,
credential-bearing URLs, mutable API paths, more than 10,000 measured requests,
or concurrency above 128.

| Target | Default URL | Request path measured |
| --- | --- | --- |
| `make load-test-gateway` | `http://127.0.0.1:5173/healthz` | nginx proxy, Axum middleware, health handler |
| `make load-test-api` | `http://127.0.0.1:8080/healthz` | direct Axum middleware and health handler |
| `make load-test-ready` | `http://127.0.0.1:5173/readyz` | nginx, Axum middleware, PostgreSQL pool checkout, `SELECT 1` |

The packaged release exposes only the gateway target. Direct port `8080` is a
contributor-only diagnostic published by the repository Compose file.

Each result reports completed requests, throughput, average/p50/p95/p99/max
latency, HTTP status counts, and missing or duplicate `X-Request-Id` values. Any
transport failure, unexpected status, missing request ID, or reused request ID
fails the command. A hardware-specific p95 budget can be supplied explicitly:

```bash
FFDB_LOAD_REQUESTS=1000 \
FFDB_LOAD_CONCURRENCY=24 \
FFDB_LOAD_MAX_P95_MS=75 \
make load-test-gateway
```

Keep a fixed command, FFDB revision, warm-cache state, and host when comparing
runs. Run each profile at least three times and compare medians rather than
treating a single sample as a capacity result. Use `--json` directly when a CI
job needs structured output:

```bash
node scripts/load-smoke.mjs \
  --url http://127.0.0.1:5173/healthz \
  --requests 1000 --concurrency 24 --warmup 24 --json
```

`make load-test-check` tests both harnesses with in-process fetch doubles and
does not require FFDB, credentials, Docker, or a network listener.

## PostgreSQL control-plane baseline

`make postgres-bench` is a separate, local-only database diagnostic. It runs a
five-second prepared `SELECT 1` pgbench profile at concurrency four, then creates
bounded temporary tables and prints before/after `EXPLAIN (ANALYZE, BUFFERS,
TIMING OFF)` plans for membership, audit-chain, backup-history, auth-session,
and commerce order-line access patterns. The EXPLAIN transaction ends with
`ROLLBACK`; it neither reads tenant values nor retains rows or indexes.

```bash
make compose-rebuild
make postgres-bench
```

Defaults are 50,000 synthetic rows per access pattern, five pgbench seconds,
and four clients. Hard caps prevent more than 50,000 rows, 30 seconds, or 32
clients:

```bash
FFDB_PG_BENCH_ROWS=20000 \
FFDB_PG_BENCH_SECONDS=10 \
FFDB_PG_BENCH_CONCURRENCY=8 \
make postgres-bench
```

Run the same command at least three times and compare median execution times and
buffer counts. The direct pgbench number measures PostgreSQL/container protocol
overhead, not the authenticated HTTP path; use `make load-test-query` for the
credential, rate-limit, audit, billing, worker IPC, and SQLite composition.

## Opt-in authenticated query profile

`make load-test-query` measures the complete authenticated project-query path,
including nginx, Axum middleware, credential verification, PostgreSQL routing
and billing context, executor admission, worker IPC, SQLite execution, metering,
audit, and response serialization. It is deliberately excluded from
`make load-test` because it changes operational and billing records.

Use a local or disposable project. Give the harness either an end-user access
token or a developer API key with `database_query` scope. Enter the token into a
hidden environment variable rather than placing it in shell arguments:

```bash
printf 'Project UUID: '
IFS= read -r FFDB_QUERY_LOAD_PROJECT_ID
printf 'Bearer token: '
IFS= read -rs FFDB_QUERY_LOAD_TOKEN
printf '\n'
export FFDB_QUERY_LOAD_PROJECT_ID FFDB_QUERY_LOAD_TOKEN
make load-test-query
unset FFDB_QUERY_LOAD_PROJECT_ID FFDB_QUERY_LOAD_TOKEN
```

The script accepts the project UUID and bearer token only from the environment.
It never includes either value in its summary, JSON output, target label, or
failure details. It refuses credential command-line options, custom paths,
custom SQL, credential-bearing URLs, and non-loopback hosts. The only request it
can issue is:

```http
POST /v1/projects/{project_id}/query
Authorization: Bearer <environment-only credential>
Content-Type: application/json

{"sql":"SELECT 1 AS ffdb_load_probe","parameters":[],"options":{"max_rows":1}}
```

The default is 100 measured requests, four warmups, concurrency four, and a
five-second timeout. Hard limits are 2,000 measured requests, 100 warmups, and
concurrency 32. Tune within those bounds without putting credentials in the
command line:

```bash
FFDB_QUERY_LOAD_REQUESTS=500 \
FFDB_QUERY_LOAD_CONCURRENCY=8 \
FFDB_QUERY_LOAD_MAX_P95_MS=100 \
make load-test-query
```

The statement cannot change project schema or tenant rows. The requests are not
side-effect-free at the platform level, however:

- every admitted query consumes project plus API-key/user execution rate-limit
  capacity; the low anonymous IP policy is reserved for setup, sign-in,
  registration, verification, refresh, invitation acceptance, and recovery;
- an admitted request writes requested and terminal audit events;
- a successful execution writes a metering event for its read usage and current
  logical database-size snapshot;
- an end-user token associates that subject with the billing period's active
  subject set, which can affect MAU reporting; and
- request and normalized query-fingerprint observability samples are retained,
  subject to the bounded channel's dropped-sample behavior.

Warmups have the same effects as measured requests. The report therefore shows
both counts and the maximum number of metered/audited/rate-limited attempts.
HTTP 429, metering denial, audit unavailability, authentication failure, missing
request IDs, and duplicate request IDs all fail the run rather than being
silently excluded from latency statistics. Do not use this target against a
production project or as a way to bypass a configured usage or rate limit.

## Interpreting the three profiles

- Gateway health minus direct API health approximates local nginx proxy and
  connection overhead. A large gap points to gateway saturation, container
  networking, access logging, or host resource contention.
- Gateway readiness minus gateway health exposes PostgreSQL pool acquisition and
  a trivial control-plane query. A large or unstable gap points to PostgreSQL,
  pool sizing, or competing control-plane work rather than SQLite execution.
- Missing or duplicate request IDs indicate a correctness failure in middleware
  composition even if every response is HTTP 200.

The default aggregate harness intentionally does not load project query routes.
Use the separate opt-in authenticated profile above only with a disposable or
explicitly approved local project; never aim the repository smoke at production.

### Profile PostgreSQL admission and audit overhead

Two ignored Rust diagnostics isolate the durable PostgreSQL work paid by an
authenticated dispatch. They require a migrated disposable local database and
are excluded from ordinary CI because workstation, Docker, and storage timing is
not a release capacity claim:

```bash
TEST_DATABASE_URL=postgres://ffdb:...@127.0.0.1:5432/ffdb \
  cargo test -p ffdb-rate-limits postgres_check_many_latency_profile \
  -- --ignored --nocapture

TEST_DATABASE_URL=postgres://ffdb:...@127.0.0.1:5432/ffdb \
  cargo test -p ffdb-audit postgres_append_latency_profile \
  -- --ignored --nocapture
```

The limiter probe uses unique project and actor digests and deletes them. The
audit probe creates an isolated schema, exercises sequential and eight-way
same-stream appends, verifies exactly one chain root and every predecessor link,
then drops the schema. It never modifies the production append-only ledger.
Compare the same command, database volume, build profile, and idle host before
and after a change; report p50, p95, and p99 together because advisory-lock tail
latency can be noisy even when median latency improves.

## Current request-path constraints

The main project query path is:

1. nginx proxies `/v1` to Axum in packaged deployments;
2. Axum assigns a request ID, records metrics, traces the request, validates JSON
   and SQL limits, and verifies credentials; only anonymous authentication and
   setup routes consume the conservative pre-authentication IP bucket;
3. PostgreSQL supplies credential and project-route state and billing context;
4. `ProcessWorkerExecutor` admits the request through a bounded global semaphore,
   resolves or starts the project worker, serializes the worker envelope to JSON,
   and exchanges a length-prefixed frame over child-process stdin/stdout;
5. the project worker runs the request through a single guarded SQLite
   connection; and
6. the API records query telemetry and usage before returning the public result.

The evidence-backed capacity constraints are therefore:

- Requests for one project serialize at the worker-process mutex and again at
  the SQLite connection mutex. This preserves SQLite transaction ordering, but
  makes a single hot project latency-bound rather than horizontally concurrent.
- The executor's queue is a global semaphore sized as worker maximum multiplied
  by configured queue capacity. When exhausted, admission fails with
  `QueueFull`; raising it increases waiting and memory rather than project
  throughput.
- Worker lookup and process creation share one worker-map mutex. The lock is
  short for warm workers, but cold starts and a high number of active project
  routes increase contention and process pressure.
- Authenticated query admission performs PostgreSQL work for credentials,
  routing, rate limits, and billing context. Pool exhaustion can dominate before
  the SQLite worker becomes busy.
- The durable limiter uses separate policies. Anonymous setup/authentication IP,
  auth-project, auth-user, and auth-API-key buckets default to a burst of 120 and
  a refill of two tokens per second. Authenticated execution project, user, and
  API-key buckets default to a burst of 2,000 and 200 tokens per second. A query
  charges the project and API-key/user dimensions independently. HTTP results
  above the execution refill rate can therefore reach `429`; that is an
  admission-policy result, not raw SQLite capacity.
- Worker requests and responses cross a JSON/framed pipe with a 9 MiB ceiling.
  Large result sets pay serialization, allocation, copying, and IPC costs in
  addition to SQLite execution.
- HTTP observability uses a bounded non-blocking channel and drops samples rather
  than blocking requests when full. A fast load run must inspect the dropped
  sample count before relying on portal percentiles from that same run.

Use the instance and project observability surfaces plus Prometheus metrics to
decide which constraint is active. Do not increase PostgreSQL connections,
worker processes, queues, or result limits together: change one bounded input,
repeat the same workload, and retain the before/after output.

### Durable rate-limit configuration

| Environment variable | Default | Purpose |
| --- | ---: | --- |
| `FFDB_RATE_LIMIT_PRE_AUTH_CAPACITY` | `120` | Burst per anonymous/authentication identity |
| `FFDB_RATE_LIMIT_PRE_AUTH_REFILL_PER_SECOND` | `2` | Sustained setup/authentication attempts per identity |
| `FFDB_RATE_LIMIT_EXECUTION_CAPACITY` | `2000` | Burst per authenticated project and actor identity |
| `FFDB_RATE_LIMIT_EXECUTION_REFILL_PER_SECOND` | `200` | Sustained authenticated operations per identity |
| `FFDB_RATE_LIMIT_IDLE_TTL_SECONDS` | `3600` | Idle PostgreSQL bucket retention |
| `FFDB_RATE_LIMIT_MAX_ENTRIES` | `1000000` | Global durable bucket-state bound |

The low policy remains distributed and fail-closed for sensitive anonymous and
authentication routes. Raising the execution policy does not weaken password,
registration, refresh, invitation, recovery, or bootstrap throttling. Tune the
execution burst/refill only from repeatable load evidence; keep the entry and
TTL bounds sized for PostgreSQL maintenance and expected identity cardinality.
