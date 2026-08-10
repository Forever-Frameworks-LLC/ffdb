# `@ffdb/sync-client`

Runtime-neutral orchestration for FFDB's logical offline-sync protocol. The
package is ESM and its runtime code uses standard JavaScript only, so the same
`OfflineSyncClient` class can run in a modern browser, React Native, or Node.js.
The runtime must also support `@ffdb/client` (or supply its `fetch`
implementation) and an authenticated end-user session.

```bash
pnpm add --save-exact @ffdb/client@0.3.6 @ffdb/sync-client@0.3.6
```

The matching GitHub Release also provides a checksum-listed
`ffdb-sync-client-0.3.6.tgz` for verified offline installation.

```ts
import { FFDBClient, MemorySessionStore } from "@ffdb/client";
import { OfflineSyncClient } from "@ffdb/sync-client";
import { IndexedDbReplica } from "@ffdb/sync-client/browser";

const api = new FFDBClient({
  baseUrl: "https://data.example.com",
  projectId: "your-project-id",
  sessionStore: new MemorySessionStore("example"),
});
await api.auth.signIn(email, password);

const replica = new IndexedDbReplica(`ffdb-${projectId}-${userId}`);
const sync = new OfflineSyncClient(api, replica);
await sync.sync();

const note = await sync.getRow("notes", noteId);
const notes = await sync.listRows("notes");
```

`MemoryReplica` is useful for tests, examples, and process-lifetime caches. It is
not durable: a reload, app termination, or Node process exit loses its rows,
pending mutations, rejected mutations, and cursor.

## What the orchestrator does

One `sync()` run is single-flight and performs these steps:

1. Fetch and transactionally replace the replica from an RLS-filtered snapshot
   when no cursor exists.
2. Push queued mutations in batches of at most 100 and transactionally remove or
   reject each pending record from the server's per-mutation result.
3. Pull from the pre-push cursor so server-authoritative changes produced by the
   accepted local mutations are stored in the replica.
4. Apply ordered upserts/deletes and the replacement cursor in one adapter
   transaction, continuing while `has_more` is true.
5. Replace the snapshot on `resnapshot_required` or `invalidate_scope`.

`mutate()` validates a mutation, then atomically queues it and applies its
insert, partial update, or delete to the visible local rows. `getRow()` and
`listRows()` therefore reflect an edit as soon as durable enqueue succeeds.
Server row version and sequence metadata remain authoritative: applied writes
are replaced by the following pull, while duplicate, superseded, and rejected
outcomes atomically invalidate the old cursor and trigger a fresh snapshot so
optimistic state cannot survive when no new logical change exists. If that
recovery snapshot is interrupted, the missing cursor forces the next sync to
retry it. Pending edits are replayed over any snapshot taken before they are
pushed.

The client exposes `idle`, `snapshot`, `push`, `pull`, and `error` phases through
`state` and `subscribe()`. An `AbortSignal` cancels the active network work. A
second concurrent `sync()` call joins the first call and therefore uses the
first call's signal.

## Runtime matrix

| Runtime | Does `OfflineSyncClient` run? | Bundled replica | Required application setup |
| --- | --- | --- | --- |
| Browser | Yes, in modern ESM/fetch runtimes | `IndexedDbReplica` from `@ffdb/sync-client/browser` | Use a database name scoped to the project and signed-in user; delete or rotate it when that authorization scope is removed |
| React Native / Expo | Yes, with compatible fetch, URL, Headers, and AbortController globals | `MemoryReplica`; durable `NativeSQLiteReplica` is exported by `@ffdb/react-native` | Supply a secure session store and a `NativeSQLiteDriver` wrapper for the chosen SQLite library |
| Node.js | Yes in ESM Node 24+ | `NodeSQLiteReplica` from `@ffdb/sync-client/node` | Give each project/user authorization scope its own protected SQLite path and close the replica during graceful shutdown |

The browser and Node adapters persist snapshots, cursors, pending mutations, and
rejections transactionally. All bundled replicas expose the same deterministic
`getRow(table, primaryKey)` and `listRows(table)` read surface, plus bounded
`getPending(limit)` and `getRejected(limit)` bookkeeping reads.
`NodeSQLiteReplica` uses Node 24's built-in
`node:sqlite`; it does not add a native npm dependency. Importing the core
package does not open a database, watch connectivity, schedule background work,
or automatically sync on focus/reconnect. React applications can subscribe with
`useSync` from `@ffdb/react`; applications normally trigger `sync()` from
explicit UI and reasonable lifecycle or network-status events.

```ts
import { NodeSQLiteReplica } from "@ffdb/sync-client/node";

const replica = new NodeSQLiteReplica("/var/lib/my-app/ffdb-user.sqlite3");
const sync = new OfflineSyncClient(api, replica);
try {
  await sync.sync();
} finally {
  await replica.close();
}
```

## Replica adapter contract

A persistent adapter must guarantee:

- `transaction()` commits all callback writes atomically and rolls them all back
  when the callback rejects;
- snapshot rows and cursor are replaced in one transaction;
- pulled changes and cursor advance in one transaction;
- pending removal/rejection and push-result processing are atomic;
- enqueue and its optimistic row insert/update/delete are atomic;
- cursor values remain opaque and are never parsed, logged, or constructed;
- `getRow()` returns one decoded record or `null`, while `listRows()` returns
  only the requested table in canonical primary-key JSON order;
- `getPending(limit)` is deterministic and returns at most `limit` records;
- `getRejected(limit)` is deterministic and retains the stable error code and
  rejection timestamp needed for user-visible resolution;
- enqueueing the same mutation ID and payload is idempotent, while reusing the ID
  for different content is rejected;
- local data is isolated and protected according to the runtime's threat model.

The local read surface deliberately stops at primary-key lookup and deterministic
table listing. It does not expose the private SQLite/IndexedDB connection or
accept arbitrary local SQL. Filter, group, and index the returned typed records
in application code, or implement a custom `ReplicaAdapter` when the product
needs a richer safe read model.

## Boundaries

- Sync is end-user-only and always remains subject to the server's current RLS.
- Server sequence, not client time, orders conflicts.
- Mutation batches contain 1–100 records; pull batches contain 1–1,000 changes.
- Rejected mutations move to adapter rejection bookkeeping and are readable
  through `getRejected()`. This package does not provide conflict-resolution UI
  or an automatic retry policy for them.
- The package does not cache arbitrary `FFDBClient.query()` results, rewrite SQL,
  expose a local SQLite connection, encrypt local storage, or migrate an
  application's own schema.
