# Runtime scaling and gateway topology

Status: implemented single-node contract for release `0.3.x`.

## Native request path

The bare-metal request path has one public gateway stage:

```text
client -> Caddy (:443) -> ffdb-api/Axum (127.0.0.1:8080)
                               |
                               +-> one lazy database-worker per active project database generation
```

Caddy also listens on `127.0.0.1:5173` for on-host acceptance checks. That is a
second listener on the same process, not another proxy. Caddy serves the compiled
landing, docs, and portal assets, obtains and renews TLS certificates, rejects the
raw metrics path, and proxies API traffic directly to Axum. Native installations
do not install or route through nginx.

The Docker release image still uses nginx as its sole compiled static gateway.
That profile binds the gateway to loopback and does not publish Axum. An operator
must not put the native Caddy service in front of that nginx image and call it the
native topology; doing so adds buffering, header, timeout, logging, and failure
boundaries without adding application capacity.

## Concurrency available now

Axum uses Tokio's multi-threaded runtime and can process independent HTTP and
PostgreSQL control-plane work concurrently in one process. SQLite execution is
isolated differently:

- `FFDB_WORKER_MAX_PROCESSES` bounds simultaneously resident database-worker
  children across projects on one node.
- A worker is created lazily for the first request to a project database and is
  eligible for eviction after five idle minutes.
- `FFDB_WORKER_QUEUE_CAPACITY` is the admission allowance per configured worker;
  the supervisor's bounded global allowance is that value multiplied by
  `FFDB_WORKER_MAX_PROCESSES`.
- One project database generation has one worker process and exchanges one framed
  request at a time. Requests to different project databases can run in parallel.

There is deliberately no `FFDB_PROJECT_CONCURRENCY` setting. The old setting was
parsed but never used, so it was removed instead of advertising concurrency that
did not exist.

## Why Axum replicas are not round-robined yet

The current `ffdb-api` binary is both an HTTP server and the local SQLite-worker
supervisor. Starting several copies for the same node and round-robining requests
would let each process create a worker for the same SQLite file and duplicate
process-local reservations, maintenance state, and telemetry workers. PostgreSQL
route generations fence stale nodes, but they do not elect one supervisor among
several processes sharing a node identity. Caddy therefore has exactly one Axum
upstream in the supported native configuration.

Safe horizontal scale requires an architectural split before adding replicas:

1. A stateless HTTP/control-plane tier that can be round-robined.
2. A separately registered worker-node agent that exclusively owns each routed
   `(database_id, generation)` and exposes authenticated internal RPC.
3. Durable node leases and leader election for singleton maintenance/reporting
   jobs.
4. Route-aware forwarding, retry/fencing semantics, and load tests that prove a
   project is never active on two worker owners for one generation.
5. Node capacity reporting based on measured queue depth, worker RSS, CPU, disk
   latency, and filesystem headroom—not request count alone.

Until those invariants exist, adding Caddy upstreams would improve neither safe
SQLite throughput nor availability. Scale the supported topology vertically,
increase the bounded worker count only after load testing representative
projects, and use the Observability page to watch queue saturation, latency, CPU,
and disk pressure.

## Client identity behind the gateway

Axum trusts forwarded client addresses only when the immediate transport peer is
inside `FFDB_TRUSTED_PROXY_CIDRS`. It walks `X-Forwarded-For` from right to left,
removes trusted proxy hops, and uses the first untrusted address for pre-auth rate
limits and audit records. Invalid, oversized, or overlong chains fail closed to
the transport peer. Native installations trust loopback only. The isolated
Docker network CIDR is explicit and must be changed together with
`FFDB_DOCKER_SUBNET` if an operator selects a different subnet.
