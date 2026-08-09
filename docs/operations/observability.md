# Observability

FFDB records a bounded 30-day performance history in PostgreSQL, exposes a
privacy-safe operator API, emits structured `tracing` spans, retains immutable
audit events, and keeps a Prometheus-compatible scrape endpoint. Open
**Observability** in the portal for the normal operating view; `/metrics` remains
available for external collection and alerting.

The portal separates two scopes:

- **Current project** includes only attributed HTTP requests, normalized query
  fingerprints, and the selected project's last sampled logical database size.
- **Entire instance** includes all routes, all projects, worker-pool saturation,
  and host database/backup filesystem capacity. It is available only to an
  instance owner or administrator and can be filtered to one project through the
  HTTP API.

Organization members can read their project view. Instance-wide telemetry
requires an instance administrator session. Observability data is operational
metadata, not an authorization source.

## Retained operator API

The API accepts `1h`, `6h`, `24h`, `7d`, or `30d`. It selects a bounded chart
resolution and returns no more than 121 time points and 20 rows in each ranked
table. New samples are aggregated in memory through a bounded channel and
flushed to PostgreSQL every five seconds. If recorder capacity is exhausted,
the response exposes `dropped_samples` instead of silently presenting the data
as complete.

```http
GET /v1/projects/{project_id}/observability?range=24h
Authorization: Bearer <platform-session>

GET /v1/instance/observability?range=7d
Authorization: Bearer <instance-admin-session>

GET /v1/instance/observability?range=7d&project_id=<uuid>
Authorization: Bearer <instance-admin-session>
```

Each summary includes request count and QPS; 4xx, 5xx, and combined error rate;
average, p50, p95, p99, and maximum latency; continuous chart buckets; busiest
and slowest stable API routes; most frequent and slowest query fingerprints;
worker and execution-slot saturation; current in-flight requests; logical
database size; and database/backup filesystem totals and available bytes.

Logical database size is sampled after successful project database operations.
Disk signals are point-in-time host filesystem readings, not historical series.
An idle project therefore reports no logical-size sample until it performs a
database operation after the observability migration is installed.

## Correlation

Each inbound request receives a UUIDv7 request id returned as `X-Request-Id` and
propagated in worker requests and audit calls made by that request. HTTP spans use
only the bounded method and stable route template; worker failures and lifecycle
logs add typed opaque identifiers where their module emits them.

Never record bearer credentials, cookies, password/template variables, provider
secrets, signed URLs, raw email bodies, database paths, raw SQL, SQL identifiers,
comments, literal values, or bound parameter values.

FFDB lexes each statement before recording it. Keywords, operators, and bounded
structure remain; identifiers, quoted identifiers, strings, numbers, blobs, and
bind parameters become `?`. The normalized shape is limited to 96 tokens and 320
characters, then SHA-256 hashed for the stable fingerprint. Query timings and row
counts come from execution inside the isolated database worker. The recorder does
not time replayed idempotency responses a second time.

This design makes frequency and latency grouping useful without turning the
telemetry store into a second copy of customer queries. Treat normalized shapes
as operational metadata and keep the endpoint access controlled nonetheless.

## Exported metrics

`GET /metrics` exports the following bounded Prometheus series:

- `ffdb_http_requests_total{method,route,status_class}`;
- `ffdb_http_request_duration_seconds{method,route}`;
- `ffdb_http_requests_inflight`;
- `ffdb_auth_failures_total{kind,reason}`;
- `ffdb_rate_limit_denials_total{dimension}`.

Because every public SQL, storage, sync, email, backup, management, health, and
integrity operation has a stable route label, external request-count,
error-rate, and latency SLOs remain available without tenant identifiers. The
Prometheus series are deliberately instance-wide; use the retained API and
portal when a project scope is required. Structured lifecycle logs and the
append-only audit stream provide request-specific diagnosis.

Prometheus labels must not include project, user, request, object key, SQL, or
token IDs. Restrict `/metrics` to the trusted operator network. In packaged
Docker installations access it through the compiled gateway at
`http://127.0.0.1:5173/metrics`; never publish Axum directly on host port 8080.

## Data lifecycle and recovery

Minute aggregates live in the control-plane PostgreSQL database. Cleanup runs
hourly and removes buckets older than 30 days. The tables have bounded keys,
fixed latency buckets, and upserts so restart/retry behavior does not create an
unbounded label space. Include PostgreSQL in normal backup and restore planning;
the organization metrics SQLite volume is billing/capacity data and does not
contain this performance history.

If the portal reports dropped samples, inspect PostgreSQL latency and execution
slot saturation before increasing capacity. If charts are empty after an
upgrade, confirm migration 14 exists, generate a project request, wait for the
next five-second flush, and retry the endpoint with the active platform session.

## Baseline alerts

Page for cross-project/routing invariant failure, repeated worker crashes,
integrity failure, restore-test failure/expiry, refresh-reuse surge, signing-key
failure, backup age beyond policy, disk exhaustion forecast, and provider outage
that exceeds the error budget. Ticket sustained route latency/rate-limit pressure,
provider capacity, checkpoint lag, email dead letters, and quota trends. Alert
rules that need a deployment signal must be wired to its PostgreSQL, filesystem,
S3, SMTP/Resend, or container exporter before production cutover.
