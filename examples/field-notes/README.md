# FFDB Field Notes

A polished localhost feature lab for the public FFDB application surface. The
React app is a real task workspace rather than a collection of disconnected API
buttons. It keeps the current user's data in an IndexedDB replica, uses remote
transactions for dependent writes, queues selected edits offline, and binds task
attachments to RLS-protected object storage.

The same example includes a Node 24 smoke workflow using the durable built-in
SQLite replica. Together they cover every runtime-neutral application feature in
`@ffdb/client`, `@ffdb/react`, and `@ffdb/sync-client` that makes sense in an
end-user project.

## What is exercised

| Surface | Evidence in the example |
| --- | --- |
| Authentication | register, verify email, sign in, refresh through the SDK, sign out, password-reset request |
| Sessions | list sessions and revoke non-current sessions |
| Parameterized SQL | owner-scoped reads with tagged parameters |
| Transactions | task plus event writes commit together |
| RLS | task, event, and storage policies compare `owner_id` with `auth.uid()` |
| Browser offline sync | `IndexedDbReplica`, optimistic update/delete, pending state, snapshot/push/pull, online resume |
| React integration | `FFDBProvider`, `AuthProvider`, `useAuth`, `useFFDB`, `useQuery`, `useSessions`, `useStorageUpload`, `useSync`, and `optimisticList` |
| Object storage | list, upload, authenticated download, delete, and multipart upload/abort for files at least 5 MiB |
| Trusted project setup | migration apply, bucket creation, schema/policy introspection, readiness, and integrity check |
| Node | `MemorySessionStore`, parameterized query, `OfflineSyncClient`, and persistent `NodeSQLiteReplica` |

Platform-owner administration, backups/restores, email-template publishing,
organization billing, and project commerce are intentionally not embedded in an
end-user browser. They are operator or provider workflows with destructive or
external side effects; the management portal and live acceptance suite remain
the right full-system test surfaces for those capabilities.

## Connect a hosted test project

Requirements:

- the hosted FFDB server must match this checkout's package version (`0.3.7`);
- one project ID;
- one project developer key with `database_migrate`, `database_schema`,
  `storage_manage`, and `backups_manage` (the last scope authorizes the
  non-destructive integrity check);
- one test user's email and password (or create one in the app);
- under the project's **Auth → Policy → Application URLs**, add the web origin
  `http://127.0.0.1:5180` and the exact auth redirects
  `http://127.0.0.1:5180/?ffdb_auth=verified` and
  `http://127.0.0.1:5180/?ffdb_auth=password-reset`;
- the storage provider's CORS allowlist must include
  `http://127.0.0.1:5180` for browser upload/download testing.
- provider bucket versioning is optional for the main walkthrough. Setup reports
  it as a limitation and creates a non-versioned private bucket when the active
  provider does not support versioning.

Keep the developer key server-side. It is read only by the Node setup command;
Vite exposes only variables prefixed with `VITE_`.

```bash
cd examples/field-notes
cp .env.example .env.local
# Fill in .env.local without committing it.

pnpm setup
pnpm dev
```

Open [http://127.0.0.1:5180](http://127.0.0.1:5180). By default the local Vite
server proxies `/v1`, `/healthz`, and `/readyz` to `FFDB_PROXY_TARGET`, avoiding
an unnecessary hosted API CORS change. Signed object requests still go directly
to the configured storage public origin, so provider CORS must allow the exact
localhost origin above.

If you deliberately want the browser to call FFDB directly, set
`VITE_FFDB_API_URL` and add the local origin to the project's **Allowed web
origins** in the portal. The change is live; it does not require SSH or a host
restart.

## Suggested walkthrough

1. Create an account, verify it using the delivered token, and sign in.
2. Let the initial transaction seed four tasks, then create and complete one.
3. Edit a task title. It changes immediately in IndexedDB and shows `Pending
   sync`; click **Sync now** to push it.
4. Upload a small file, download it, and delete it. Try a 5–50 MiB file to run
   the multipart path.
5. Open **Sessions** in another tab after signing in there, then revoke the
   older session.
6. Open **Diagnostics** and run the user-safe checks.
7. Sign out, create a second user, and confirm the first user's tasks and
   attachments are invisible under RLS.
8. Run the Node runtime check:

   ```bash
   pnpm smoke:node
   ```

The Node check creates one optimistic row in a user-scoped SQLite replica,
pushes it to FFDB, verifies it with a parameterized remote query, deletes it,
and retains the local replica under `.data/` so persistence can be inspected.

## Local verification

```bash
pnpm check
pnpm test
pnpm build
```

The example is also a workspace package, so the root `pnpm check`, `pnpm test`,
and `pnpm build` commands include it.
