# TypeScript SDKs and CLI

## Packages

- `@ffdb/client`: the only TypeScript package that speaks HTTP. It supports
  browser, Node, and compatible fetch runtimes; developer queries, migrations,
  schema and policies; end-user auth; ordered typed results; storage signing;
  snapshot/pull/push; platform organizations/projects/API keys; auth settings;
  logs and backups; cancellation; retries; and idempotency. Tagged worker
  envelopes are normalized before results reach callers. Its trusted
  platform-session surface also covers public first-run discovery, owner
  bootstrap, instance deployment/provider configuration, global
  administrators and inventory, billing exemptions, and plan catalog.
- `@ffdb/sync-client`: offline replica orchestration with durable IndexedDB
  storage for browsers, built-in SQLite storage for Node 24+, a transactional
  adapter contract for other runtimes, typed local row reads, atomic optimistic
  mutations, rejection inspection, and a memory adapter reserved for tests.
- `@ffdb/react`: providers and hooks for auth, query state, sync state, sessions,
  optimistic list updates, and storage upload state.
- `@ffdb/react-native`: session persistence without browser APIs and an adapter
  for Expo/native SQLite drivers.
- `@ffdb/cli`: strict migration parsing/checksums plus credential login/logout,
  organization/project/API-key/linking, SQL/seed/development, schema/policy,
  migration, auth, storage, email, logs, backup, integrity, health, and complete
  `ffdb instance` operator lifecycle commands.
- `@ffdb/email-components`: versioned React Email defaults and their allowed
  variable manifest.

## Client example

Every tagged FFDB release publishes six version-matched packages under the
public `@ffdb` npm scope with provenance and includes checksum-listed `.tgz`
copies in the matching GitHub Release. Application developers do not need an
FFDB source checkout.

```sh
VERSION=0.3.6
npm install --save-exact \
  "@ffdb/client@$VERSION" \
  "@ffdb/sync-client@$VERSION" \
  "@ffdb/react@$VERSION"
npm install --global "@ffdb/cli@$VERSION"
```

```ts
import { BrowserSessionStore, FFDBClient } from "@ffdb/client";

const ffdb = new FFDBClient({
  baseUrl: "https://data.example.com",
  projectId: "019fc39c-ddbd-7d12-9849-e4ee35310132",
  sessionStore: new BrowserSessionStore(sessionStorage, "my-app.ffdb"),
});

await ffdb.auth.signIn(email, password);
const result = await ffdb.query({
  sql: "select id, title from documents where title like ?1 order by title",
  parameters: [{ type: "text", value: `${search}%` }],
  options: { max_rows: 100 },
});
```

The server's RLS policy scopes this end-user query; application code should not
treat a manually supplied owner filter as authorization. Project developer keys
remain server/operator credentials and must never be embedded in browser or
native bundles.

SQL parameters match the Rust tagged representation: `null`, `integer`, `real`,
`text`, or base64 `blob`. Results preserve column order and duplicate names by
returning rows as arrays. Integers outside JavaScript's safe range remain decimal
strings and BLOBs use `{ "$blob": "..." }`.

The client deduplicates concurrent access-token refreshes and retries an
unauthorized end-user request once after rotating the refresh token. It never
retries developer credentials automatically. Provide `AbortSignal` and an
`Idempotency-Key` for cancellable/state-changing work.

A runnable public-API example is included as `scripts/live-e2e.mjs`; from a
healthy local Compose stack, `pnpm test:live` covers SDK auth, parameterized SQL,
RLS, storage, sync, email, backups, and platform administration without importing
server internals.

The CLI resolves explicit flags, environment variables, then its credential
file. In a terminal, `ffdb login` securely prompts for email and a masked
password. For automation, `FFDB_PASSWORD=... ffdb login developer@example.com
--json` exchanges the password for the API's rotating platform session and
writes the returned opaque session with owner-only file permissions.
Organization/project management and API-key
issuance use that platform session; database administration uses the separately
scoped project API key. `ffdb logout` revokes the platform session and deletes the
local file. Output never prints a complete stored credential unless the server
intentionally returns a newly created API-key secret once.

For a fresh host, `ffdb instance setup-status` is unauthenticated and reveals
only bootstrap/capability flags. `ffdb instance bootstrap <owner-email>` reads
`FFDB_BOOTSTRAP_TOKEN` and `FFDB_PASSWORD`, persists only the returned platform
session, and does not print credential plaintext. Instance BYO setup similarly
reads `FFDB_INSTANCE_STRIPE_SECRET_KEY` and
`FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET` without storing or printing them. The CLI
then exposes authenticated instance status/configuration, Connect
onboarding/refresh, organization-creation policy, administrators, paginated
global users/organizations, billing exemptions, and plan catalog. Revocations,
global user/organization enablement changes, and plan retirement preserve the
CLI's destructive confirmation behavior and are audited by the API.

## CLI migration format

```sql
-- migrate:up
CREATE TABLE documents (id TEXT PRIMARY KEY, owner_id TEXT, title TEXT);

-- migrate:down
DROP TABLE documents;
```

The filename is `<stable_id>_<name>.sql`. The CLI hashes id, name, up SQL, and
down SQL with NUL separators to match the Rust protocol. Both directions are
mandatory; rollback never attempts inverse-DDL generation.

```sh
ffdb --project "$FFDB_PROJECT_ID" --key "$FFDB_DEVELOPER_KEY" migration apply migrations/20260802_documents.sql
ffdb --project "$FFDB_PROJECT_ID" --key "$FFDB_DEVELOPER_KEY" policies --json
```
