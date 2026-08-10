# `@ffdb/react-native`

Runtime-neutral storage contracts for using FFDB from React Native or Expo. This
package does not depend on a particular native storage or SQLite library. It
exports:

- `ReactNativeSessionStore`, which adapts an asynchronous key/value store to the
  `@ffdb/client` end-user `SessionStore` contract;
- `NativeSQLiteReplica`, a durable `ReplicaAdapter` for
  `@ffdb/sync-client`;
- `AsyncKeyValueStorage`, `NativeSQLiteDriver`, `SQLiteResult`, and
  `SQLitePrimitive`, which applications implement for their chosen runtime.

It does not bundle Expo SecureStore, AsyncStorage, Expo SQLite, a fetch polyfill,
or React components.

```bash
pnpm add --save-exact @ffdb/client@0.3.10 @ffdb/sync-client@0.3.10 \
  @ffdb/react-native@0.3.10
```

The matching GitHub Release also provides checksum-listed `.tgz` files for
verified offline installation.

## Secure end-user sessions

Refresh and access tokens are credentials. Back `ReactNativeSessionStore` with
OS-protected secure storage, not plain AsyncStorage. For Expo SecureStore, adapt
the method names explicitly:

```ts
import * as SecureStore from "expo-secure-store";
import { FFDBClient } from "@ffdb/client";
import {
  ReactNativeSessionStore,
  type AsyncKeyValueStorage,
} from "@ffdb/react-native";

const secureStorage: AsyncKeyValueStorage = {
  getItem: (key) => SecureStore.getItemAsync(key),
  setItem: (key, value) => SecureStore.setItemAsync(key, value),
  removeItem: (key) => SecureStore.deleteItemAsync(key),
};

const client = new FFDBClient({
  baseUrl: process.env.EXPO_PUBLIC_FFDB_API_URL!,
  projectId: process.env.EXPO_PUBLIC_FFDB_PROJECT_ID!,
  sessionStore: new ReactNativeSessionStore(secureStorage),
  // Supply fetch here only when the runtime does not provide a compatible one.
});
```

Only public API URL/project identifiers belong in `EXPO_PUBLIC_*` variables.
Never put a developer API key, password, access token, or refresh token there.
Changing Expo environment files requires restarting the development server.

The store removes malformed or structurally invalid persisted session JSON and
returns `null`. Storage availability, biometric/access-control policy, backup
behavior, device migration, token size limits, and at-rest protection remain the
application's responsibility.

## Native SQLite replica

```ts
import { OfflineSyncClient } from "@ffdb/sync-client";
import { NativeSQLiteReplica } from "@ffdb/react-native";

const replica = new NativeSQLiteReplica(nativeSQLiteDriver);
await replica.initialize();

const sync = new OfflineSyncClient(client, replica);
await sync.sync();

const draft = await sync.getRow("drafts", draftId);
const drafts = await sync.listRows("drafts");
```

`nativeSQLiteDriver` is an application-supplied wrapper. Its contract is more
important than the library-specific method names:

- `execute(sql, parameters)` binds positional parameters and returns rows as
  objects plus a `changes` count;
- `transaction(work)` gives the callback a driver bound to one real SQLite
  transaction, commits only after the callback resolves, and rolls back every
  callback write when it rejects;
- the SQLite build must support STRICT tables and the documented `ON CONFLICT DO
  UPDATE ... WHERE` statements;
- calls made through the callback driver must not escape or interleave outside
  that transaction.

`initialize()` creates private `__ffdb_client_*` metadata, row-cache, pending,
and rejection tables. It is also called lazily. Do not expose those tables to
untrusted SQL or treat their JSON representation as the application's schema.
The adapter stores snapshot replacement, pulled changes, mutation bookkeeping,
cursor advances, and optimistic local mutations atomically. It implements the
same typed `getRow(table, primaryKey)`, deterministic `listRows(table)`,
`getPending(limit)`, and `getRejected(limit)` APIs as the browser, Node, and
memory replicas. These methods return decoded records without exposing the
private SQLite connection. Applications that need arbitrary local queries can
implement `ReplicaAdapter` alongside a richer safe read model.

`NativeSQLiteReplica` is not proof that every native SQLite wrapper satisfies the
contract. Test rollback, parameter binding, row-object conversion, concurrent
transactions, app restart, and the exact SQLite version on every supported
platform before claiming durable offline behavior.

## Networking and lifecycle

`OfflineSyncClient` performs network work through the supplied `FFDBClient`; this
package never calls fetch itself. Use an HTTPS API URL outside explicit localhost
development, pass a compatible `fetch` implementation if the runtime lacks one,
and handle network errors at the sync boundary. The packages do not subscribe to
NetInfo or AppState. Applications decide when to pause, resume, and call
`sync()`, and may pass an `AbortSignal` when a screen or task is cancelled.
