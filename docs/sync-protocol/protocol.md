# Offline sync protocol

FFDB synchronizes logical row changes, never SQLite WAL frames. The server owns
the order, schema version, transaction id, row version, commit epoch, and opaque
cursor. Client timestamps are diagnostic metadata and never participate in
conflict resolution.

## Snapshot and cursor

Start a replica with `GET /v1/projects/{project_id}/snapshot`. The response has a
schema version, an opaque authenticated cursor, and RLS-filtered tables encoded as
ordinary ordered query results. Clients store the snapshot and cursor in one
local SQLite transaction.

A cursor is bound to the project, authenticated subject, registered client/device,
schema version, authorization-scope fingerprint, server sequence, issue time, and
expiry. It is not a sequence number for clients to parse or construct. Invalid,
overlong, expired, cross-project, cross-subject, changed-scope, and compacted
cursors fail closed with `sync.resnapshot_required` where recovery is possible.

## Pull

```http
GET /v1/projects/PROJECT_ID/sync?cursor=OPAQUE&limit=1000
Authorization: Bearer USER_JWT
```

The server walks committed changes after the cursor, evaluates SELECT RLS for
each upsert and retained delete tombstone, and returns a new cursor. An empty
visible batch may still advance the cursor. Continue while `has_more` is true.
Control events require either scope invalidation or a complete resnapshot.

Application writes executed through the normal query and transaction operations
are captured atomically by trusted physical-table triggers. A successful write and
its logical change record therefore commit together; clients do not need a
separate sync-specific write endpoint to make server-originated changes visible.

## Push and deterministic LWW

```json
{
  "schema_version": 4,
  "mutations": [{
    "mutation_id": "ios-01:019fc4c2",
    "table": "documents",
    "primary_key": "doc-42",
    "operation": "update",
    "values": {"title": "Offline edit", "owner_id": "USER_ID"},
    "base_row_version": 8,
    "client_timestamp_ms": 1785686400000
  }]
}
```

The verified access token id is the trusted client binding; any body-supplied
client id is ignored. The token id plus subject plus `mutation_id` form the
idempotency namespace. Reusing an id with different content is rejected. Duplicate
submissions are reauthorized before a neutral duplicate/applied result is returned;
an old receipt cannot bypass a new policy. Idempotency records have bounded
capacity and retention and are pruned during compaction.

Last-write-wins means the last mutation accepted by the server has the greatest
server sequence. An update after a delete recreates the row with a later version;
a delete after an update leaves a retained tombstone. Duplicate primary-key
inserts follow the same server ordering. Client wall-clock time never wins.

In partial-batch mode the database worker evaluates and commits each mutation
independently and returns an applied, duplicate, or rejected result for every
mutation. A stale diagnostic `base_row_version` does not override server arrival
order. Invalid rows and RLS failures do not prevent independent valid mutations
in the same request from being applied.

The current protocol exposes only partial-batch behavior and returns
per-mutation rejection codes. RLS rejection, schema mismatch, and invalid
mutations never advance that mutation's row version.

## Durable server state

`ffdb-sync-engine` exports a versioned opaque checkpoint containing logical rows,
ordered changes, tombstones, and idempotency receipts; cursor signing keys are
never included. Restore validates the project, format version, sequence ordering,
row versions, and configured bounds before accepting state.

`ffdb-sync-worker::SyncStore` is a compare-and-swap persistence boundary. Every
push, invalidation, and compaction must durably advance its checkpoint revision
before success is returned; failure restores the prior in-memory state. The
filesystem implementation uses fsync plus atomic rename for a single service.
The project database stores ordered changes, row versions, tombstones, transaction
ids, actors, schema versions, and client mutation ids in protected SQLite internal
tables. These records update in the same trusted transaction as application-row
mutations. A separate process-only map is never the source of truth.

## Tombstones, policy changes, and compaction

Deletes retain the prior RLS-relevant row data for 90 days by default so an
offline client cannot resurrect a deleted row. Change cursors retain 30 days by
default. A policy, claims scope, or incompatible schema change appends a control
event and changes the scope/schema fingerprint. When incremental delivery cannot
retract bytes already cached locally, the client must destroy the affected local
replica and fetch a new snapshot.

`@ffdb/sync-client` performs snapshot replacement, pending mutation push, ordered
pull application, cursor updates, optimistic row writes, and rejection
bookkeeping inside adapter transactions. Its typed primary-key read and
deterministic table-list APIs are consistent across bundled replicas. A
non-applied push outcome clears the old cursor atomically before authoritative
resnapshot, so interrupted recovery remains fail-closed. `@ffdb/react-native`
supplies a runtime-neutral native SQLite adapter; it does not assume DOM,
`localStorage`, or browser globals.
