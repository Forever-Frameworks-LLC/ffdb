# Scaling

PostgreSQL is the source of routing/lifecycle truth, but API replicas also own
durable per-organization SQLite usage ledgers below `FFDB_METRICS_ROOT`.
Therefore the current packaged topology is one API owner for each organization
metrics root; it is not safe to add interchangeable API replicas that write
independent copies of an organization's ledger. Database workers scale by active
project load and require durable node-local/block storage for routed SQLite
files.

The supported native request path and the exact boundary that prevents safe
round-robin API replicas are documented in
[runtime scaling and gateway topology](../architecture/runtime-scaling.md).

## Capacity model

Plan independently for API RPS, PostgreSQL connections, active SQLite projects,
per-project serial execution time, worker processes, disk IOPS/capacity, logical-log volume,
organization usage-event/outbox volume, object bytes/egress, and asynchronous
email/backup work. Total registered projects must not determine an unbounded
connection or permission cache.

The supervisor has a bounded global admission semaphore sized from the configured
per-worker allowance, and each project worker exchanges one request at a time.
Idle connections and workers evict with a cap and deadline. A hot project can be moved by acquiring its
lifecycle lock, draining it, copying/restoring through a verified safe operation,
and publishing a new fenced route generation. Old nodes reject later requests.

## Horizontal scaling

- Add API replicas only after assigning each organization to one fenced metrics
  owner and routing every usage-producing request consistently to that owner.
  The packaged single-node profiles do not supply this coordinator.
- Split stateless HTTP admission from the stateful worker supervisor and add
  authenticated route-aware RPC before putting Axum replicas behind round robin.
- Size PostgreSQL pools so the sum of all replica maxima leaves administrative
  and maintenance headroom; use a transaction pooler only after SQLx behavior is
  verified.
- Partition database nodes by explicit route metadata, not a caller-computed path.
- Scale sync/email workers from durable queue age with bounded attempts and dead
  letters.
- Keep S3/provider rate limits and presigning latency in the admission budget.

Never raise global limits to solve a single tenant's workload. Increase its
explicit quota or isolate it on a dedicated node. Schema migrations, restores,
backups, and compaction remain serialized per project at every scale.
