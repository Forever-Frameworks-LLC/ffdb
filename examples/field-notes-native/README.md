# FFDB Field Notes Native

An Expo / React Native companion to the browser Field Notes example. It uses
FFDB's public client packages exactly as an application should: no developer
key, database connection string, or storage-provider credential is bundled.

## What it exercises

| Surface | Native workflow |
| --- | --- |
| Authentication | Register, verify, sign in, reset password, refresh, sign out |
| Secure persistence | `ReactNativeSessionStore` backed by Expo SecureStore |
| SQL and RLS | `auth.uid()` in parameterized queries and atomic transactions |
| Offline sync | Per-user `NativeSQLiteReplica`, waiting pulls, retry backoff, and AppState resume |
| Local mutations | Queue offline task notes and deletes, then push automatically |
| Storage | Sign, upload, commit, list, sign, and download a generated text Blob |
| Sessions | List active sessions and rotate the current token pair |
| Diagnostics | Readiness, authenticated query, sync, storage, and sessions |

The app expects FFDB `0.3.14` or newer because that release enables the
documented `auth.uid()`, `auth.role()`, `auth.jwt()`, and `auth.claim()` calls in
application SQL as well as RLS policies.

## Project setup

The native and browser examples share one project schema and private storage
bucket. Apply that trusted setup once from the browser example, using a scoped
developer key only in its Node environment:

```bash
cd examples/field-notes
cp .env.example .env.local
# Fill in FFDB_API_URL, VITE_FFDB_PROJECT_ID, and FFDB_DEVELOPER_KEY.
pnpm setup

cd ../field-notes-native
cp .env.example .env.local
# Fill in only the public API URL and project ID.
```

Only these browser-safe values belong in the Expo environment:

```dotenv
EXPO_PUBLIC_FFDB_API_URL=https://your-ffdb-host.example.com
EXPO_PUBLIC_FFDB_PROJECT_ID=your-project-id
```

Never add an FFDB developer key, password, access token, or refresh token to an
`EXPO_PUBLIC_*` variable.

In the portal, open the selected project at **Auth → Policy → Application
URLs** and add this exact link to **Allowed auth redirects** (the right-hand
list, not **Allowed web origins**):

```text
ffdb-field-notes://auth/callback
```

Native `fetch` calls are not governed by browser CORS. If you also run the Expo
web target, add the exact origin printed by Expo (commonly
`http://localhost:8081`) as an allowed web origin. Web storage requests can
also require that origin in the object provider's own CORS policy.

Custom-scheme email callbacks are best tested in an iOS/Android development
build; Expo Go does not own the app's production scheme.

The app starts the same runtime-neutral automatic-sync controller used by the
browser and Node examples. `AppState` pauses network work in the background and
triggers an immediate catch-up on resume. Network failures back off and retry;
an application that already uses NetInfo can additionally call
`controller.setOnline(...)` to pause retries while connectivity is known to be
unavailable.

## Run it

```bash
cd examples/field-notes-native
./script/build_and_run.sh
```

Useful direct modes:

```bash
./script/build_and_run.sh --ios
./script/build_and_run.sh --android
./script/build_and_run.sh --web
./script/build_and_run.sh --doctor
```

The project-local Codex environment exposes matching **Run**, **Run iOS**,
**Run Android**, **Run Web**, and **Expo Check** actions from
`.codex/environments/environment.toml`.

## Verification

```bash
pnpm --filter @ffdb/example-field-notes-native check
pnpm --filter @ffdb/example-field-notes-native exec expo export --platform ios
pnpm --filter @ffdb/example-field-notes-native exec expo export --platform android
pnpm --filter @ffdb/example-field-notes-native export:web
```

The web build uses Expo SQLite's WASM worker. `metro.config.js` includes WASM as
an asset; a web host must also serve the cross-origin isolation headers required
by Expo SQLite.
