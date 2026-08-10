# `@ffdb/react`

React providers and hooks layered over `@ffdb/client` and
`@ffdb/sync-client`. The package can be used by React DOM or React Native when
the underlying client/session/replica adapters support that runtime.

```bash
pnpm add --save-exact @ffdb/client@0.3.7 @ffdb/sync-client@0.3.7 \
  @ffdb/react@0.3.7 react
```

The matching GitHub Release also provides checksum-listed `.tgz` files for
verified offline installation.

```tsx
import type { PropsWithChildren } from "react";
import { AuthProvider, FFDBProvider } from "@ffdb/react";

export function Providers({ children }: PropsWithChildren) {
  return (
    <FFDBProvider client={client}>
      <AuthProvider>{children}</AuthProvider>
    </FFDBProvider>
  );
}
```

Exports include `useFFDB`, `useAuth`, `useQuery`, `useSessions`,
`useStorageUpload`, `useSync`, and `optimisticList`.

## Sync state

```tsx
function SyncButton() {
  const state = useSync(offlineSyncClient);
  return (
    <button disabled={state.phase !== "idle"} onClick={() => void state.sync()}>
      {state.phase === "idle" ? "Sync" : state.phase}
    </button>
  );
}
```

`useSync` uses React's external-store contract to subscribe to an existing
`OfflineSyncClient`. It exposes the current phase, pending count, last-sync time,
error, and an explicit `sync(signal?)` function. It does not construct a replica,
start background work, monitor browser focus/AppState/NetInfo, or sync on mount.

On the web, provide a browser-compatible `FFDBClient` and a persistent
`ReplicaAdapter` if reload durability is required. On React Native, use secure
credential storage and a verified native adapter such as `NativeSQLiteReplica`
from `@ffdb/react-native`. In Node-based React rendering, do not start a sync run
during server rendering; create runtime-specific clients and adapters outside
the render path.

The query and storage hooks call the remote FFDB API. They are not local-replica
queries and do not become offline automatically merely because `useSync` is also
present in the component tree.
