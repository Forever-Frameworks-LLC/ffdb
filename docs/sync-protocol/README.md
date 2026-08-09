# Logical Offline Sync Protocol

FFDB sync is an ordered logical change protocol, not SQLite WAL replication.
Clients may keep a local SQLite replica through a platform adapter.

## Snapshot and pull

A snapshot returns the current schema version, RLS-visible tables/rows, and an
opaque cursor at a consistent server sequence. Pull returns changes strictly after
that cursor plus a replacement cursor. Cursors are authenticated, size bounded,
project/schema/scope bound, expire after retention, and must never be parsed or
logged by clients.

Every change records server sequence, transaction id, table, primary key,
operation, row version, new values or tombstone, actor, schema version, server
commit epoch, and optional client mutation id. Pull re-evaluates SELECT RLS. If a
policy/scope/schema change cannot remove previously cached data incrementally, the
server sends `resnapshot_required`/`invalidate_scope`; the client must destroy the
affected replica and snapshot again before serving it.

## Push and conflicts

Each mutation carries a unique mutation id, operation, primary key, values, base
row version, and optional diagnostic client timestamp. Idempotency is bound to the
verified subject and access-token id. Reuse with different content is
rejected. RLS is evaluated on every new mutation and before disclosing a stored
duplicate receipt.

Last-write-wins always uses server-assigned commit sequence. Client clocks never
order conflicts. The later server sequence wins for update/update, update/delete,
and delete/recreate. A duplicate primary-key insert follows the same deterministic
server mutation path. Deletes create tombstones retained longer than the maximum
offline window so stale clients cannot resurrect rows.

Batches report per-mutation results and commit each accepted mutation
independently. Schema mismatch, expired cursor, policy change, or compacted
history can require resnapshot. Compaction removes changes, tombstones, and
idempotency records only after their independent retention horizons.
