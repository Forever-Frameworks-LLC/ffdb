# FFDB manual acceptance test plan

## Purpose and release gate

This plan validates a release candidate through FFDB's public surfaces: the
current-source Docker stack, landing site, documentation site, management portal,
HTTP API, TypeScript packages, CLI, PostgreSQL control plane, isolated project
SQLite databases, MinIO, Mailpit, and durable volumes. It supplements rather than
replaces the automated suites.

Run the plan against a dedicated acceptance environment. Do not run destructive
restore, revocation, or volume-reset tests against production or shared customer
data. A release is a **go** only when the criteria in [Go/no-go](#go-no-go) are
met and the evidence packet contains no credential, token, signed URL, email
action token, provider key, or unredacted secret.

The expected behavior comes from these sources:

- [HTTP API reference](../API/reference.md) and [OpenAPI contract](../API/openapi.json)
- [Local development](../operations/local-development.md)
- [RLS semantics](../rls-semantics/README.md)
- [Logical sync protocol](../sync-protocol/README.md)
- [Object storage and RLS](../object-storage/README.md)
- [Email templates](../auth/email-templates.md)
- [Backup and restore](../operations/backup-restore.md)
- [Client SDK](../client-sdk/README.md) and [CLI](../cli/README.md)
- Published-package guidance for [the client](../../packages/client/README.md)
  and [the CLI](../../packages/cli/README.md)
- The public-surface [live E2E harness](../../scripts/live-e2e.mjs)

If this plan and the running OpenAPI document disagree, stop, record the release
commit and request ID, and classify the contract drift before continuing. Do not
guess at a route or weaken an expected authorization result.

## Acceptance record

Complete this header in the evidence packet.

| Field | Value |
| --- | --- |
| Release/version | |
| Git commit and dirty status | |
| Tester(s) | |
| Start/end time and timezone | |
| Host OS and architecture | |
| Docker/Compose versions | |
| Rust/Node/pnpm versions | |
| Desktop browsers | |
| Mobile browsers/devices | |
| API/portal/landing/docs origins | |
| Compose image IDs | |
| Evidence directory or ticket | |
| Known accepted defects | |
| Final decision and approvers | |

For every numbered test record `Pass`, `Fail`, `Blocked`, or `N/A`, the tester,
time, expected and actual result, and evidence references. `N/A` requires an
approved reason; it must not be used to bypass a release-critical capability.

## Severity and triage

| Severity | Definition | Examples | Release effect |
| --- | --- | --- | --- |
| P0 critical | Active security boundary failure, unrecoverable corruption/loss, credential disclosure, or cross-tenant/cross-user access | RLS leaks Bob's row to Alice; raw SQLite/provider credentials are returned; restore corrupts the only accepted backup | Immediate stop and no-go; preserve evidence and follow the incident process |
| P1 high | A primary advertised workflow is unusable or materially unsafe without a reasonable workaround | Cannot bootstrap/sign in, provision a project, query, verify auth, upload, sync, or restore; current source is not what Docker runs; critical keyboard path is blocked | No-go unless the feature is explicitly removed from the release scope and independently approved |
| P2 medium | Material defect with a safe documented workaround; limited browser/responsive/accessibility degradation | One secondary portal action fails while the CLI works; table needs awkward horizontal scrolling; non-critical focus restoration defect | Requires owner, documented workaround, and explicit release approval |
| P3 low | Cosmetic or minor content issue with no material task impact | Spacing, non-blocking truncation, minor copy mismatch | May ship with an owner and target release |

Any result that only appears to pass because a developer credential bypassed an
end-user path is a failure. Any screenshot/HAR/log attachment containing a secret
is handled as a P0 evidence incident even if the product itself redacted other
outputs correctly.

## Prerequisites and safety

### Required workstation and services

- Rust 1.96.1 with rustfmt and Clippy.
- Node.js 24 or newer and pnpm 11.6.0.
- Docker Engine/Desktop and Compose v2.
- `curl`; `jq`, `shasum`, and browser developer tools are strongly recommended.
- A desktop viewport of at least 1440 × 900 and a narrow viewport near 390 × 844.
- Current Chrome/Chromium, Firefox, and Safari/WebKit. Include a real iOS Safari
  or Android Chrome device when the release claims mobile-browser support.
- A screen reader appropriate to the host (VoiceOver, NVDA, or JAWS), keyboard
  only, a contrast checker, and browser emulation for reduced motion and 200%
  zoom.
- A secure evidence location with access restricted to the release team.

### Test identities and fixture names

Generate a unique non-secret run suffix such as `20260802-scotter-01`. Use it in
every mutable name so acceptance records are distinguishable from the automated
E2E records.

Prepare:

- one platform owner, using a unique email and a password of at least 12
  characters;
- one organization `Manual Acceptance <run>` and slug `ma-<run>`;
- one project `Manual Acceptance <run>` and the same unique slug;
- two end users, Alice and Bob, with distinct email addresses and passwords;
- one invited developer in an email address the tester can retrieve from Mailpit;
- one full-scope project API key and one deliberately restricted key;
- a small UTF-8 text file, a zero-byte file, and a multipart-sized binary fixture;
- one precompiled verification email artifact;
- one sentinel database row and one encrypted backup created before the sentinel
  is modified.

Store plaintext API keys and passwords only in a password manager or in-memory
shell variables. Never put them in source files, screenshots, command history,
issue text, or `VITE_*` variables. Use private/incognito profiles for Alice and
Bob so their `sessionStorage` cannot be confused.

### Required browser matrix

Run the complete critical path in the primary release browser. Run the marked
smoke path in every other cell.

| Surface | Desktop | Narrow/mobile | Required smoke path |
| --- | --- | --- | --- |
| Landing | Chrome, Firefox, Safari | iOS Safari and/or Android Chrome | Navigation, anchors, CTA targets, animations/reduced motion, no horizontal page overflow |
| Docs | Chrome, Firefox, Safari | iOS Safari and/or Android Chrome | Navigation, search, theme, code copy, direct URL, back/forward, 404 |
| Portal | Chrome, Firefox, Safari | iOS Safari and/or Android Chrome | Developer sign-in, project selection, every navigation item, SQL read, tables/forms, confirmation and error state |
| SDK fixture | Chrome and Firefox | primary mobile browser | End-user sign-in, authenticated query, refresh/reload, storage CORS |

## Evidence rules

For each section capture only what is needed to prove the outcome:

- terminal transcript with command, exit status, and timestamps;
- release commit, `docker compose ps --all`, `docker compose images`, and image IDs;
- screenshots at desktop and narrow widths, including the URL and viewport;
- a redacted network record with status, route template, response shape, and
  `X-Request-Id`; strip `Authorization`, cookies, bodies containing passwords,
  token responses, and signed URL query strings;
- schema version, row counts, object key/size, backup UUID/status/hash prefix,
  and audit action/outcome, without protected row contents where unnecessary;
- accessibility notes including keyboard order, accessible name, screen-reader
  announcement, contrast result, zoom/reflow, and reduced-motion result;
- defect link for every failure or blocked step.

Never capture the API-key creation secret, developer/end-user sessions, Mailpit
action tokens, password reset links, signed S3 URLs, or complete backup SHA-256.
Record only a safe prefix or the fact that the value was present once.

## Phase 1: current source, static gates, and Docker

### ENV-01 — Record the candidate and toolchain

1. From the repository root, record `git rev-parse HEAD`, `git status --short`,
   `rustc --version`, `node --version`, `pnpm --version`, `docker version`, and
   `docker compose version`.
2. Confirm the commit is the approved candidate. Explain every local change.
3. Run `make bootstrap`.

Expected:

- the required tools are present;
- locked Cargo and pnpm dependencies resolve;
- Compose configuration validates;
- an existing `.env` is not overwritten;
- a missing `.env` is created only from `.env.example` and contains disposable
  local values, not production secrets.

### ENV-02 — Run release static and automated gates

Run, in order:

```sh
make verify
```

Expected: formatting, Clippy, Rust tests, TypeScript builds/checks/tests,
documentation link checks, router/OpenAPI drift checks, Compose validation, and
final builds all exit zero. Save the summary, not secret-bearing environment
output. Any skipped workspace package must be explained.

### ENV-03 — Rebuild Docker from the candidate

```sh
make compose-rebuild
make status
docker compose images
```

Expected:

- the API, PostgreSQL, MinIO, Mailpit, sync worker, and unified landing/docs/portal
  web gateway reach their documented healthy/completed state;
- the API and web-gateway containers were recreated after the start of this test;
- image IDs/timestamps are recorded and differ from a deliberately older
  candidate where applicable;
- the current checkout was the Docker build context;
- `minio-bootstrap` and `volume-init` exit successfully rather than remaining as
  unhealthy daemons;
- all published development ports bind to `127.0.0.1`, not every interface.

This is the source-of-truth container path. Merely restarting an older image is
not an acceptance result.

### ENV-04 — Health, contract, and observability smoke

Request:

```sh
curl --fail --include http://127.0.0.1:8080/healthz
curl --fail --include http://127.0.0.1:8080/readyz
curl --fail --include http://127.0.0.1:8080/openapi.json
curl --fail --include http://127.0.0.1:8080/metrics
```

Expected:

- health reports `ok` and readiness reports `ready`;
- API responses include a fresh `X-Request-Id`;
- OpenAPI is 3.1 and includes the route set documented in the API reference;
- metrics include `ffdb_http_requests_total` without secrets, SQL text, email
  action tokens, signed URLs, or raw provider identifiers;
- malformed requests produce the stable `{ "error": { "code", "message",
  "request_id" } }` shape without internal paths or SQL details.

With a platform session and project ID, generate several reads, writes, one
failing statement, and two statements with the same structure but different
identifiers and values. Wait at least five seconds, then request:

```sh
curl --fail \
  -H "Authorization: Bearer $FFDB_PLATFORM_SESSION" \
  "http://127.0.0.1:8080/v1/projects/$FFDB_PROJECT_ID/observability?range=1h"
```

Expected:

- request totals, continuous time-series buckets, route latency percentiles,
  worker saturation, and storage signals are present;
- the project response excludes requests attributed to a second project;
- the structurally equivalent statements share one SHA-256 fingerprint;
- raw SQL identifiers, comments, string/number literals, and parameter values
  do not appear anywhere in the JSON response or PostgreSQL telemetry tables;
- an organization non-member receives 403, while an instance administrator can
  read `/v1/instance/observability` and filter it with `project_id`;
- an unsupported range returns 400 with `observability.range_invalid`;
- after process restart, the pre-restart window remains visible, while records
  older than 30 days are removed by retention cleanup.

### ENV-05 — Public live baseline

```sh
make live
```

Expected: the harness rebuilds TypeScript packages and current-source containers,
then passes organization/project/key creation, migrations and RLS, two-user auth,
storage, sync conflicts/replay/tombstones, email delivery, backup/restore,
invitation, rotation, audit/metrics/CLI, and revocation. It must not print a
bearer token, API key, signed URL, or action token. Preserve its TAP-like summary.

The manual phases remain required; `make live` cannot establish visual,
responsive, cross-browser, assistive-technology, or human confirmation behavior.

## Phase 2: verify the unified web gateway

The current-source Compose image serves all three Vite applications through one
nginx gateway at `http://127.0.0.1:5173`:

- landing: `http://127.0.0.1:5173/`;
- documentation: `http://127.0.0.1:5173/docs/`;
- management portal: `http://127.0.0.1:5173/app/`.

Use these integrated URLs for acceptance. Open a nested documentation URL such
as `/docs/quickstart` directly and reload it; nginx must return the docs SPA, not
the landing page or a 404. Reload `/app/` and confirm the portal remains under
that prefix. Confirm `/docs` and `/app` redirect to their trailing-slash forms,
assets load from `/docs/assets/` and `/app/assets/`, and `/v1`, `/healthz`,
`/readyz`, and `/openapi.json` still proxy to the FFDB API. `/metrics` must
return 404 at the gateway and remain available only on the private API listener.

Standalone Vite mode is optional for component development only. When needed,
run the portal, landing, and docs dev servers on `127.0.0.1:5173`, `:5174`, and
`:5175` respectively. Their root-mounted local preview behavior is not evidence
for integrated cross-application routing and does not replace the gateway checks.

### WEB-01 — Landing desktop

At 1440 × 900:

1. Confirm the title, description, FFDB brand, hero, architecture marquee, four
   capabilities, three-step workflow, architecture, facts, integrations,
   security, developer examples, deployment choices, final CTA, and footer load.
2. Use the primary navigation links for Capabilities, Architecture, and Security.
   Each must move to the correct heading without hiding it beneath the header.
3. Exercise all developer code tabs and Copy. The active tab has
   `aria-selected="true"`, only the selected code is copied, and the UI confirms
   the copy without changing layout.
4. Verify docs, quickstart, portal, client, CLI, license, security, backups, and
   observability links have the expected destination. Confirm no link points to
   an unpublished or private source repository.
5. Scroll down and back up. The header remains legible, reveal content does not
   remain permanently hidden, canvases/SVGs do not cover interactive content,
   and there is no console exception.

Expected: copy and links work, product claims match current package names and
architecture, current self-hosting is clearly available, private/team modes are
clearly separated from monetized BYO/Connect instances, the operator-owned
Free/PAYG/Pro contract is consistent with the billing docs, and the page has no
horizontal document overflow. Published `@ffdb/*` package examples must resolve
to the documented npm packages and must not depend on workspace-only imports.

### WEB-02 — Landing narrow/mobile

At 390 × 844 and one real mobile device:

1. Open the menu, verify `aria-expanded`, follow an anchor, reopen it, and close
   with Escape where a keyboard is available.
2. Scroll every section; verify cards, code, tables/lists, marquee, and footer do
   not clip meaningful content.
3. Rotate the device. Confirm controls remain reachable and content does not jump
   behind the fixed menu.
4. Enable `prefers-reduced-motion: reduce`. The animated sphere, marquee,
   rotating hero word, reveal transitions, and decorative motion must stop or
   become effectively static. Record any animation that continues.

### WEB-03 — Documentation desktop

1. Open **Install → Docker** first. Confirm the primary path starts with a
   complete, copyable `compose.yaml`, a matching `.env`, secret-generation
   commands, and direct `docker compose config`, `pull`, `up`, `logs`, and
   upgrade/stop commands. Copy both files into an empty directory and run
   `docker compose --env-file .env -f compose.yaml config --quiet`. It must
   succeed without a source checkout. Confirm the page explains why the normal
   installation is Compose rather than a misleading single-container
   `docker run`, and that the signed lifecycle helper is presented as an
   optional alternative instead of the only installation path.
2. Open the introduction directly, then visit at least one page in every group:
   Start here, Install, Database, Auth and storage, Sync and offline, SDKs and
   tools, Operations, and Reference. Confirm Docker Compose and systemd are both
   directly visible in the sidebar and mobile drawer.
   Open Quickstart and confirm it contains only the four task sections: install,
   verify, create the owner/project, and connect an application. It must not
   repeat release pinning, air-gap, lifecycle, DNS, or operations material from
   the dedicated guides.
3. Expand/collapse groups. The current page and section links remain visible and
   expose correct `aria-current`/`aria-expanded` state.
4. Follow section anchors and previous/next links. Headings must use contextual
   task language rather than the repeated `What, why, and when` template, and
   clickable headings must not display a trailing `#`. Refresh a nested URL and
   use browser Back/Forward. The same page and document title must be restored.
5. Open search by click and by Command/Ctrl+K. Search for `RLS`, `multipart`,
   `backup`, and a nonsense string. Verify relevant results, empty results,
   initial focus, Escape dismissal, backdrop dismissal, and no background action
   while the dialog is modal.
6. Toggle dark/light theme, reload, and confirm the choice persists in
   `localStorage` without storing any credential.
7. Inspect shell, TypeScript/TSX, SQL, JSON, environment, nginx, and systemd code
   where present. Confirm keywords, strings, comments, variables/properties, and
   punctuation receive distinguishable syntax treatment in both themes without
   changing copied text. Copy a block and confirm only its raw code is copied.
8. Visit a nonexistent docs route and confirm the in-app 404 and return action.

Expected: every navigation entry resolves, content matches the current API and
package names, Docker/systemd commands match the shipped artifacts, code samples
contain no real secret, and the browser console has no missing asset, routing,
or hydration errors.

### WEB-04 — Documentation narrow/mobile

At 390 × 844:

1. Open/close the navigation drawer by button, Escape, backdrop, and following a
   link. Verify the button state and focus remain understandable.
2. Search with the on-screen keyboard open. Results, close control, and selected
   result remain reachable without two-dimensional scrolling.
3. Verify code blocks scroll internally, prose reflows, section anchors are not
   obscured, and previous/next links remain distinguishable.
4. Test theme, 200% zoom, and both portrait and landscape orientation.

## Phase 3: first-run instance ownership, deployment mode, and project fixture

Run the first-run tests against a fresh disposable PostgreSQL database. To test
all mutually exclusive deployment modes, either use four isolated deployments
or take a verified pre-bootstrap host backup and restore it between mode tests.
Do not delete tables by hand to simulate a fresh installation.

For the repository Compose fixture, the guarded reset command is:

```bash
FFDB_CONFIRM_FRESH=DELETE_LOCAL_FFDB_DATA make compose-fresh
```

It destroys this checkout's local Compose volumes. Never run it against an
instance whose data must be retained.

### SETUP-00 — Public setup discovery and capability gating

1. Before creating any user, open `/app/` and request
   `GET /v1/instance/setup/status` without credentials.
2. Confirm the portal opens the owner-creation step rather than a generic login.
3. Confirm BYO Stripe and Stripe Connect are advertised as supported onboarding
   choices without any provider secret already installed on the host.
4. Select each platform mode and confirm the wizard requires the owner to enter
   its Stripe secret key and webhook signing secret without storing either in
   browser storage or returning either from the API.

Expected:

- setup discovery returns no owner identifier, email, provider account ID, key,
  or other secret;
- only the first-user bootstrap state and non-secret mode availability are
  public;
- unavailable paid modes are disabled with an actionable prerequisite instead
  of accepting a form that must fail later;
- private and team modes remain available without Stripe configuration.

### SETUP-01 — Bootstrap exactly one platform owner

On a fresh acceptance control-plane database, use the `/app/` first-run form to
submit the one-time bootstrap token, owner email, password, and confirmation.
Separately verify the underlying `POST /v1/developer/bootstrap` contract with
the token in `X-FFDB-Bootstrap-Token` and JSON `{ "email", "password" }`. Use a
secure API client that redacts request headers and bodies. The disposable local
default token may be used only on the isolated local Compose stack.

Expected:

- first use returns HTTP 201, signs the browser in, and advances directly to the
  deployment-mode step; the session plaintext is not captured in evidence;
- the same transaction creates exactly one immutable instance owner and one
  matching owner administrator row;
- the owner email is normalized and the password disappears from the UI/request
  tool after submission;
- a second bootstrap attempt returns 409 `already initialized`, does not create
  another owner, and includes a request ID;
- an incorrect bootstrap token returns a generic credential failure without
  revealing whether the platform is initialized.

On a retained environment where bootstrap already occurred, record the expected
409 and sign in with the existing acceptance owner instead.

### SETUP-01A — First-run deployment-mode matrix

Exercise each mode on an isolated fresh fixture:

1. **Private workspace:** choose owner-only organization creation and provide no
   payment configuration.
2. **Team installation:** choose invitation-only creation, invite another user,
   and provide no payment configuration.
3. **Platform with BYO Stripe:** use Stripe test-mode secret and webhook keys;
   verify the configured Product, recurring Prices, Meters, currency, and Pro
   base amount match the active FFDB catalog before submission.
4. **Platform with Connect:** start Connect onboarding, complete all required
   test-account details, return to `/app/`, refresh provider state, and finish
   with charges/payouts/details enabled.

Expected:

- private/team finish without a payment provider, leave tenant billing
  enforcement off, and still record usage for capacity planning;
- BYO credentials are encrypted in PostgreSQL, never returned, and activate the
  provider only after all catalog objects are verified on that Stripe account;
- Connect returns only an allowlisted `https://*.stripe.com` onboarding URL,
  scopes every billing request to the connected account, and cannot become
  enabled until Stripe reports the required capabilities;
- a catalog price, currency, recurrence, meter, or account mismatch rejects the
  mode transaction instead of displaying one price and charging another;
- project commerce remains unconfigured and independent in every instance mode.

### SETUP-01B — Owner reconfiguration and failure recovery

1. As the owner, switch team → BYO → private, then private → Connect on fixtures
   with valid Stripe test configuration and no non-canceled organization billing
   accounts.
2. Inject a provider timeout during each paid transition and retry with the same
   idempotency key, then with a new key.
3. Attempt the same changes as a delegated instance administrator.
4. After switching from a paid mode to private, restart the API and attempt an
   organization checkout plus a usage-reporting delivery.
5. In a paid fixture, create an active organization subscription and attempt to
   switch modes, replace the BYO key with one for a different Stripe account,
   and rotate to a key for the same Stripe account. Then cancel and reconcile
   every organization billing account and retry the mode change.

Expected:

- the owner can reconfigure after first run without recreating the instance;
- a failed provider validation leaves the previous durable mode and active
  provider unchanged; retries do not create duplicate Connect accounts;
- delegated administrators can operate allowed global resources but cannot view
  or rotate owner payment credentials or change the deployment mode;
- leaving a paid mode disables enforcement, clears obsolete encrypted provider
  material, and cannot continue reporting or charging through a stale in-memory
  provider after restart;
- a non-canceled organization billing account blocks mode or Stripe-account
  replacement with 409 `instance.billing_in_use`; a same-account BYO secret
  rotation succeeds, and a mode change becomes available only after every
  organization subscription is canceled and reconciled.

### SETUP-01C — Global instance administration

1. Create users and organizations, then inspect the Instance page's paginated
   user and organization inventory.
2. Grant one user instance-admin access, sign in as that user, then revoke it.
3. Confirm the owner cannot be revoked, demoted, disabled through this surface,
   or replaced by a second bootstrap.
4. Change organization-creation policy through every value and test admission
   with owner, admin, ordinary authenticated, invited, and anonymous actors.
5. Make one organization billing-exempt with a reason, cross its Free limits,
   remove the exemption, and repeat in another organization.
6. Edit Free/PAYG/Pro limits and behaviors, retire an unused paid plan, and try
   to retire Free or an in-use paid plan.

Expected:

- global lists never expose passwords, sessions, API keys, billing secrets, or
  raw per-user metering identifiers;
- role and creation-policy changes are enforced by the API, not only the UI;
- exemptions are organization-scoped, audited, reversible, and bypass charging
  while leaving usage analytics visible;
- active plan edits drive enforcement and checkout only after Stripe price
  verification; protected/in-use plan retirement fails atomically.

### SETUP-02 — Portal developer sign-in

1. Open the portal in a clean browser profile with no active project.
2. Confirm the Welcome back screen shows the API origin, email/password labels,
   password visibility control, first-installation guidance, and OpenAPI link.
3. Submit an incorrect password. Expect a generic accessible alert and no user or
   credential enumeration.
4. Sign in as the platform owner. Expect Settings, an expiry time, and no session
   token displayed in the DOM, URL, console, or network log.
5. Reload the tab. The developer session remains for that tab through
   `sessionStorage`. Close the acceptance tab/window and open a clean one; sign-in
   is expected again.

### SETUP-03 — Organization and project

In Settings:

1. Create the uniquely named organization. Verify it appears with role `owner`.
2. Create the uniquely named project. Wait until its state is usable/active and
   verify exactly one project entry exists for the operation.
3. Select `Use project`. Confirm the project/organization labels, project ID,
   configured status, and success toast update without a full page reload.
4. Reload. The selected project persists for the tab and no secret appears in
   `localStorage`.
5. Retry equivalent organization/project creation with conflicting slugs.
   Expect a safe conflict, not a duplicate resource or internal database error.

Record the organization and project IDs; these are identifiers, not credentials.

### SETUP-04 — API-key lifecycle and scope separation

1. Issue the portal's standard scoped API key. Confirm its plaintext is shown
   exactly once, copy it to the password manager without taking a screenshot,
   then choose `I saved it`.
2. Reload and list keys. Only name, prefix, scopes, timestamps, and status may be
   visible. The secret must not be recoverable.
3. Confirm the standard portal key contains database query/migrate/schema, auth,
   storage, email, backup, and logs scopes, and does **not** claim a scope it was
   not issued.
4. Create a separate full-scope key through the CLI or SDK, including
   `keys_rotate`, and a restricted `database_schema`-only key. Keep the active
   portal key separate from the sacrificial key used for revocation tests.
5. Use the restricted key for schema (success) and query/storage/logs (403 or
   equivalent stable denial). A missing scope must never become a 200 with
   partial protected data.
6. Revoke the sacrificial key after a confirmation prompt. Its next request must
   fail immediately; list output shows `Revoked`; repeated revocation is safe.

The portal's standard generated key currently omits `keys_rotate`. Clicking
`Rotate JWT signing key` with that key must fail safely and must not show a false
success. To prove successful rotation, configure a separately issued key with
`keys_rotate` through the supported SDK/CLI/runtime path. If the candidate claims
portal-only rotation, absence of a credential-selection path is a P1 product gap.

### SETUP-05 — Link the CLI

Use a dedicated acceptance config path and enter passwords/secrets without shell
history exposure:

```sh
FFDB_PASSWORD="<entered securely>" node packages/cli/dist/main.js \
  --url http://127.0.0.1:8080 --config <acceptance-config> login <owner-email>
node packages/cli/dist/main.js --config <acceptance-config> project link <project-id>
```

Supply the full-scope key with a secure environment variable for project
operations. Expected: the credential file is owner-readable/writable only (mode
0600 on POSIX), login output omits the session token, and project linking writes
no key into a migration or scaffold file.

### SETUP-06 — Apply the manual schema and RLS policies

Create a CLI migration with explicit up/down sections. Use a unique filename and
the following logical content:

```sql
-- migrate:up
CREATE TABLE documents (
  id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL DEFAULT '',
  payload BLOB,
  created_at TIMESTAMP NOT NULL
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE documents FORCE ROW LEVEL SECURITY;

CREATE POLICY documents_owner ON documents
  AS PERMISSIVE FOR ALL TO authenticated
  USING (owner_id = auth.uid())
  WITH CHECK (owner_id = auth.uid());

CREATE POLICY storage_buckets_authenticated ON storage_buckets
  AS PERMISSIVE FOR SELECT TO authenticated USING (1);
CREATE POLICY storage_objects_owner ON storage_objects
  AS PERMISSIVE FOR ALL TO authenticated
  USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid());
CREATE POLICY storage_uploads_owner ON storage_uploads
  AS PERMISSIVE FOR ALL TO authenticated
  USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid());
CREATE POLICY storage_versions_owner ON storage_versions
  AS PERMISSIVE FOR ALL TO authenticated
  USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid());

-- migrate:down
DROP POLICY storage_versions_owner ON storage_versions;
DROP POLICY storage_uploads_owner ON storage_uploads;
DROP POLICY storage_objects_owner ON storage_objects;
DROP POLICY storage_buckets_authenticated ON storage_buckets;
DROP POLICY documents_owner ON documents;
ALTER TABLE documents DISABLE ROW LEVEL SECURITY;
DROP TABLE documents;
```

Run migration apply, status, schema, and policies through the CLI. Expected:

- apply succeeds with a stable idempotency key/checksum;
- replaying the identical migration returns the same safe outcome without a
  second application;
- editing an already-used migration ID produces a 409 checksum conflict;
- schema version increases; `documents` reports RLS enabled and forced;
- all five policies appear and no protected `__ffdb_*` backing object is exposed
  as an application table.

### SETUP-07 — Register and verify Alice and Bob

For each user, call anonymous project registration through `@ffdb/client` or
`POST /v1/projects/{project_id}/auth/register`. With verification required:

1. Registration returns a safe user ID and `verification_required: true`.
2. Mailpit at `http://127.0.0.1:8025` receives one verification message for the
   intended recipient. Retrieve the action token privately.
3. Verify through the SDK or `/auth/verify`; a replay is rejected safely.
4. Sign in and confirm access/rotating refresh credentials are returned only to
   the client session store.
5. Open isolated browser profiles for Alice and Bob.

Record each user ID for RLS fixture creation, but do not record any token.

## Phase 4: portal functional acceptance

Run PORTAL-01 through PORTAL-12 at desktop width in the primary browser. Repeat
the navigation, one read, one write, one confirmation, and one error state at
narrow width and in each secondary browser.

### PORTAL-01 — Shell, navigation, and overview

1. Confirm every navigation item allowed by the active role is present: Overview,
   Projects, Members, SQL Editor, Database, Migrations, Policies, Auth, Storage,
   Sync, Email, Activity, Backups, Usage, Products, Orders, Subscriptions,
   Settings, Account, and—only for an instance owner or administrator—Instance,
   Billing, and Users.
2. Select every item by keyboard and pointer. `aria-current="page"`, heading, and
   visible panel must agree. A route already represented in the sidebar must not
   be repeated as a second Projects/Members or Products/Orders/Subscriptions tab
   bar inside the page.
3. On Overview confirm health/readiness, schema/table version, RLS policy count,
   storage bucket count, backups, request/activity information, sync chart,
   worker status, quick actions, and activity pagination render without invented
   values.
4. If a key lacks optional logs/backups/storage scopes, the Overview must show a
   clear partial-access message while health and allowed sections continue to
   work.
5. Exercise Docs, CLI copy, notifications, Create menu, chart legends, quick
   actions, pagination, and toast dismissal.
6. Switch between light and dark mode, hard reload, and navigate every route.
   The selected theme must persist and apply before first paint; record any white
   flash, green-tinted neutral surface, unreadable disabled control, or route that
   resets the theme.
7. In SQL Editor and Migrations, confirm the CodeMirror background, gutters,
   selection, completion menu, and syntax tokens update immediately with the
   portal theme; the editor must not remain dark in light mode.

Expected: copied CLI text contains API URL/project ID but never a project key;
menus close after selection; unavailable metrics are labeled rather than shown
as zero; no loading state persists indefinitely.

### PORTAL-02 — SQL and result encoding

With the end-user session signed out, use the developer key:

1. Run `SELECT sqlite_version() AS version`.
2. Run a parameter-free result containing duplicate aliases, NULL, a safe
   integer, a 64-bit integer beyond JavaScript's safe range, REAL, TEXT, and BLOB.
3. Confirm ordered column metadata and array rows preserve duplicate names;
   unsafe integers are decimal strings, BLOB is a tagged base64 object, NULL is
   JSON null, and truncation state is represented.
4. Run two semicolon-terminated statements together. Both must execute in order
   inside one transaction and the results must remain attributable to their
   statement. Introduce an error in the second statement and confirm the first
   statement is rolled back. Explicit transaction-control statements remain
   forbidden and return a stable denial with request ID.

Then sign in as Alice in Auth and return to SQL Editor. Queries now use Alice's
verified end-user session. Insert Alice's row with all values explicitly supplied
and query `documents`; only Alice's row is visible. Attempt DDL and direct access
to a guessed `__ffdb_*` backing object; both must fail closed.

### PORTAL-03 — Database and migration history

1. Confirm Migrations opens as a full-height workbench with attached New
   migration and History tabs, not a floating tab pill plus an inset card. The
   editors and history table should use the available width and height.
2. Confirm `documents`, its exact logical SQL, RLS Enabled, Force Yes, current
   schema version, and applied migration history.
3. While Alice is signed in, Browse `documents`; only Alice's RLS-visible rows
   appear. Sign out and repeat under the developer key: because RLS is forced,
   no policy bypass is expected.
4. Apply a temporary reversible migration through the CLI. Refresh/reopen the
   panel and confirm schema/history update. Roll it back with explicit
   confirmation and verify the temporary table disappears.

### PORTAL-04 — Policies and negative RLS proof

1. Confirm policy names, table, command, roles, and enabled status.
2. Use `Create with SQL`; it should open SQL Editor with a policy template, not
   silently execute it.
3. In Alice's profile insert/query Alice's row. In Bob's profile insert/query
   Bob's row. Each profile must see exactly its own row.
4. Attempt to insert a row whose `owner_id` is the other user, update/delete the
   other user's ID, and query the other user's ID. Writes are denied or affect
   zero authorized rows, and no protected value is disclosed.
5. Verify unsupported RLS syntax is rejected during migration rather than
   falling back to an unprotected table.

Any cross-user row or object visibility is P0.

### PORTAL-05 — Auth settings, users, and sessions

1. Record the current auth settings. Toggle registration, verification required,
   minimum password length, access TTL, and refresh TTL one at a time; save and
   reload after each. Invalid/unsafe values must be rejected. Restore the release
   defaults before leaving the test.
2. Confirm Alice and Bob appear without password hashes or token material and
   with correct verified/active state.
3. Disable Bob after the confirmation. Bob's existing access must fail promptly;
   new sign-in must not succeed. Re-enable Bob and prove sign-in succeeds.
4. Sign Alice in, list sessions through the SDK, revoke the current session, and
   prove the next protected operation fails. Sign in again for later phases.
5. Start a password reset for an existing and nonexistent address. Public
   responses must be enumeration-safe; only the existing address receives mail.
   Complete Alice's reset and prove prior sessions are invalid.
6. Refresh once, then replay the pre-rotation refresh token through a private API
   client. Expect 401 and revocation of that refresh family.

### PORTAL-06 — Storage and CORS

1. As developer, create a uniquely named private bucket with versioning disabled.
   It appears with visibility, max object size, quota, and object count.
2. As Alice, upload the UTF-8 fixture under a nested logical key. Confirm tracked
   bytes, key, size, content type, and update time.
3. Download in a new `noopener,noreferrer` tab and compare bytes. Overwrite the
   same key and confirm the new bytes are returned.
4. As Bob, list the bucket and request Alice's key. Bob must see no Alice metadata
   and receive no signed download operation.
5. Attempt keys containing `..`, backslash, control characters, empty segments,
   and reserved internal prefixes. Each fails before provider access.
6. Exercise zero-byte and multipart upload/complete/abort through the SDK, then
   call the bounded storage cleanup operator action. It returns only `removed`
   and `retried` counts.
7. Delete Alice's object through the portal, first canceling and then accepting
   the confirmation. Listing and download must immediately stop exposing it.
8. Inspect the browser network record: MinIO CORS allows the portal origin,
   signed operations are short-lived/method-bound, redirects are not followed,
   and signed URL query strings are excluded from evidence and application logs.

### PORTAL-07 — Logical sync

1. Without an end-user session, Fetch snapshot and Pull changes must show a clear
   auth error with no developer-key fallback.
2. Sign in as Alice. Fetch snapshot and confirm schema version, opaque cursor,
   and only Alice-visible `documents` rows. Never copy the cursor into evidence.
3. Pull from the returned position; changes are ordered and the replacement
   cursor is opaque.
4. Through `@ffdb/sync-client`, queue an offline insert, sync, update with an old
   client timestamp, replay the same mutation ID, and delete. Expect server-order
   last-write-wins, `duplicate` replay status, and a tombstone that prevents stale
   resurrection.
5. Change schema/policy after retaining a cursor. Pull must return
   `resnapshot_required`/`invalidate_scope`; the replica is destroyed and rebuilt
   before local rows are served again.
6. Bob's snapshot and pulls must never contain Alice rows or storage metadata.

### PORTAL-08 — Email artifact, preview, publish, delivery

1. Create a verification artifact with a unique version, exact SHA-256 of its
   source, subject/HTML/text templates, and only `project_name`, `action_url`, and
   `expires_in` in `allowed_variables`.
2. Import its JSON. Confirm validated status, version, variables, and safe compile
   diagnostics. Tamper with the source without updating the digest and expect
   rejection.
3. Preview using valid scalar variables. Confirm subject/HTML/text substitution,
   HTML escaping, and rejection of undeclared variables, unsafe URL schemes,
   header CR/LF, and oversized content.
4. Cancel Publish once, then accept it. Published time updates and the prior
   version remains in history.
5. Register a new user and inspect Mailpit. The delivered subject/body uses the
   published version, contains the correct recipient-safe action, and no API key
   or internal provider response.
6. Confirm the portal states that provider credentials are deployment-managed
   and provides no UI/API response that reads or changes the Mailpit/Resend key.

### PORTAL-09 — Audit logs and request correlation

1. Open Activity after successful, denied, and failed operations.
2. Confirm chronological entries include safe time, actor, action, resource, and
   outcome for migration, auth disable, storage, backup, restore, key rotation,
   and revocation where applicable.
3. Correlate one denial with its API `X-Request-Id` and structured container log.
4. Search output and container logs for known secret prefixes and signed URL
   markers using a secure local process. No secret or protected SQL value may be
   present.

### PORTAL-09A — Organization billing and project commerce

1. In Billing, inspect actual read/write/storage/MAU usage, included allowances,
   reporting health, billing exemption state, invoices, and current period.
2. As an organization admin create PAYG and Pro Stripe test Checkouts; as a
   viewer repeat. Open the Customer Portal only after the organization has a
   provider customer.
3. In Payments, configure project BYO Stripe keys, then switch to Connect on a
   separate test project and complete onboarding. Verify only write-only
   credential status and capability/requirement details are shown.
4. Create one-time and monthly products/prices. Archive/retire them and verify
   inactive catalog filtering. Confirm the portal never accepts a browser-authored
   price during checkout.
5. Create and complete one-time and recurring test Checkouts. Inspect order,
   payment, subscription, and entitlement state only after verified webhooks.
6. Move fulfillment through processing/fulfilled, issue a partial refund, cancel
   a subscription at period end, and reload the portal after every mutation.

Expected: organization billing and project commerce remain visibly distinct;
role checks are API enforced; no redirect is presented as payment confirmation;
all provider mutations are idempotent; and the portal performs the complete
admin lifecycle without displaying secret keys or card data.

### PORTAL-10 — Backup, mutation, restore, and integrity

1. As Alice create a sentinel row/value and query it.
2. Request a backup. Confirm the portal shows created time, `complete` status,
   nonzero ciphertext size, safe hash metadata through the API, and no raw path.
3. Confirm the file on the backup volume begins with the FFDB encrypted format,
   not the SQLite header. Do this through an approved operator check without
   copying the file into evidence.
4. Modify the sentinel after the backup and prove the new value is visible.
5. Select Restore, cancel the destructive confirmation, and prove nothing
   changed. Select it again, accept, and wait for `restore_verified`/successful
   integrity response.
6. Query the sentinel: the pre-backup value is restored. Run CLI backup integrity
   and expect `ok: true` with no messages.
7. Confirm post-backup writes were not accidentally replayed, schema version is
   correct, incompatible sync clients resnapshot, and the project returns to an
   active/usable state.

Never test restore against the only copy of important data. A backup that has not
passed a restore drill is not accepted as verified.

### PORTAL-11 — Settings, membership, and key rotation

1. Invite the acceptance developer as viewer/developer. Confirm Mailpit delivery,
   accept using a new password through the SDK/API, and verify membership.
2. Change the role through every allowed transition. The invited user gains and
   loses corresponding organization/project actions without needing a leaked
   owner credential.
3. Cancel member removal, then accept it. The removed member loses access and the
   owner remains; last-owner removal must fail safely.
4. With a separately issued `keys_rotate` API key, rotate the JWT signing key.
   Expect a new active key ID, an audit event, continued validation during the
   documented overlap, and no private signing material in the response.
5. Verify the runtime configuration panel always redacts the API key to a short
   prefix and never writes the key into built JavaScript or `localStorage`.

### PORTAL-12 — Error, cancellation, and degraded states

1. Temporarily stop or block the API in a controlled window. Navigate panels and
   verify loading becomes a bounded error state explaining API/project/scope and
   request-ID troubleshooting; no infinite spinner or page crash.
2. Restore the API and retry/reopen. Reads recover without a browser restart.
3. Trigger 400, 401, 403, 404/concealed, 409, 413, and 429 where practical.
   Messages are safe and stable codes drive behavior; `Retry-After` is honored.
4. Navigate away during a slow read. The aborted request must not overwrite the
   newly selected panel or surface an unhandled rejection.
5. Confirm a non-idempotent unkeyed mutation is not automatically replayed after
   a network failure.

## Phase 5: TypeScript package acceptance

### PKG-01 — Build and inspect publish tarballs

Run `make sdk-packages` into an empty acceptance directory. It must build and
pack `@ffdb/client`, `@ffdb/sync-client`, `@ffdb/react`,
`@ffdb/react-native`, `@ffdb/email-components`, and `@ffdb/cli` at exactly the release version. Inspect
with `tar -tf`, verify `SDK-SHA256SUMS`, and install every tarball into a clean
fixture.

Expected client tarball:

- package metadata, Apache-2.0 declaration, README, ESM entry point, declarations,
  declaration maps, `generateId`, and `./package.json` export;
- no source secrets, workspace-only paths, test files, or undeclared runtime
  dependencies;
- `import { FFDBClient, BrowserSessionStore, generateId } from "@ffdb/client"`
  resolves in Node and a bundler.

Expected CLI tarball:

- executable `ffdb` entry, declarations/library exports, README, and browser,
  React, and Node templates;
- no compiled tests or embedded credential;
- workspace dependency protocols are converted to publishable package versions
  by the intended pnpm publish path.

Expected integration tarballs:

- sync-client exports its runtime-neutral core plus explicit `./browser` and
  `./node` entry points without pulling Node built-ins into a browser bundle;
- React and React Native resolve the same version of client/sync-client rather
  than a workspace path or floating range;
- all six package manifests and the release tag match, and no archive contains
  `workspace:` dependencies, test fixtures, source secrets, or repository-only
  paths.

Run `npm pack --dry-run --json` with a writable disposable npm cache as a second
manifest check. Record file names/sizes, not tarball contents that include local
credentials.

### PKG-02 — Node client usage

In a clean Node 24 ESM fixture installed from the tarball:

1. Construct `FFDBClient` with API URL, project ID, and full-scope developer key
   read only from a server-side environment variable.
2. Call health, readiness, schema, policies, a parameterized SELECT, transaction,
   logs, and integrity check.
3. Use `generateId("manual_")`; confirm a UUIDv4-shaped unique value and rejection
   of an unsafe prefix such as `../../`.
4. Omit the developer key and confirm developer-only calls fail locally/safely.
5. Pass an `AbortSignal` to a request and confirm cancellation.

Expected: ordered result arrays and tagged values match the HTTP contract, no key
is printed or bundled, and the process exits cleanly without hidden background
work.

### PKG-03 — Browser client usage

Create a clean Vite TypeScript fixture, run `ffdb init <fixture> browser`, install
the client tarball, and integrate the generated `src/ffdb.ts`.

1. Inspect `.env.example`: it may contain API URL/project ID placeholders but no
   developer key.
2. Configure only non-secret `VITE_FFDB_API_URL` and `VITE_FFDB_PROJECT_ID`.
3. Sign in as Alice, query `documents`, refresh the page, sign out, and verify
   session transitions and RLS.
4. Exercise a browser upload/download so provider CORS and the same supplied
   `fetch` path are tested.
5. Build production assets and search them/source maps for the known developer
   key and bootstrap token; neither may occur.

### PKG-04 — React usage

Create a clean React/Vite fixture, run `ffdb init <fixture> react`, and install the
client and React package tarballs.

1. Confirm generated `FFDBProviders.tsx` composes `FFDBProvider` then
   `AuthProvider` around children.
2. Add a small component using `useAuth`, `useFFDB`, and `useQuery`.
3. Verify loading → anonymous → authenticated → signed-out states are announced
   and do not cause serial duplicate requests.
4. Query Alice's rows, force an error, refetch, unmount during an active request,
   and confirm stale state is not committed.
5. Call a hook outside its provider and confirm the documented developer error,
   not an opaque runtime crash.

### PKG-05 — Client negative compatibility boundaries

Confirm the candidate does not falsely advertise or expose legacy
`@ffdb/client/react`, `@ffdb/client/node`, asynchronous `createClient`, Better Auth,
Kysely, passkey, or Stripe behavior. React, React Native, and offline sync are
separate packages. A missing legacy import must fail with clear package exports,
while documented `@ffdb/*` imports work.

### PKG-06 — Offline sync runtime matrix

Pack and install `@ffdb/sync-client`, `@ffdb/react`, and `@ffdb/react-native`
alongside the client tarball in isolated fixtures.

1. In Node 24, construct `OfflineSyncClient` with `MemoryReplica`, enqueue one
   mutation, and run snapshot/push/pull against the acceptance project. Restart
   the process and confirm memory state is intentionally gone. Repeat with the
   shipped `NodeSQLiteReplica`; restart the process and confirm rows, cursor,
   pending mutations, and rejected-mutation records survive with WAL enabled.
   Before syncing, confirm `getRow()` and `listRows()` expose optimistic insert,
   partial-update, and delete results in deterministic primary-key order.
2. In a browser fixture, repeat the memory-adapter smoke, then use the shipped
   `IndexedDbReplica`. Confirm the same durable state survives reload, tab close,
   and browser restart; verify `getRow()`, `listRows()`, `getPending()`, and
   `getRejected()` return decoded public records without exposing IndexedDB
   internals. Verify failed transactions do not partially advance the cursor or
   leave an optimistic row without its matching pending mutation.
3. In the React fixture, pass the same sync client to `useSync`; verify snapshot,
   push, pull, idle, and error states render and duplicate concurrent `sync()`
   calls share one run.
4. In React Native/Expo, adapt the runtime SQLite driver to
   `NativeSQLiteReplica`, persist the session with `ReactNativeSessionStore`,
   restart the app, and verify rows/cursor/pending mutations survive. Render a
   screen from `listRows()` while offline and confirm insert, update, and delete
   mutations appear immediately after durable enqueue.
5. In Node with an application-provided durable adapter, verify the same
   transactional adapter contract as the bundled Node SQLite implementation.
6. Force `resnapshot_required`, `invalidate_scope`, a rejected mutation, and an
   aborted request. Confirm the adapter transaction leaves rows, cursor, pending,
   and rejected state consistent. A rejected or superseded optimistic edit must
   be replaced by the authoritative snapshot while its rejection code and
   timestamp remain available through `getRejected()`. Interrupt that corrective
   snapshot once; confirm the cursor remains invalidated and the next `sync()`
   retries the snapshot before reporting idle.

Expected: the sync algorithm is runtime-neutral; browser, Node, and React Native
each have a documented durable adapter; memory remains an explicit test/demo
choice; all shipped replicas expose the same typed local read and mutation
bookkeeping surface; and application-provided adapters can implement the same
atomic contract without depending on a browser or native global.

## Phase 6: CLI acceptance

### CLI-01 — Help, login, precedence, and logout

1. Run `ffdb --help`; verify every displayed command exists and init/typegen are
   included.
2. Log in with `FFDB_PASSWORD`, then list organizations. Confirm JSON output is
   parseable with `--json` and human output is readable otherwise.
3. Verify precedence: explicit flag, environment variable, credential file,
   default. Use harmless alternate API URLs/project IDs and never expose a key in
   process listings.
4. Check the credential path has mode 0600 and atomic replacement behavior.
5. Log out. The platform session is revoked best-effort and the local credential
   file is removed. A subsequent platform operation requires sign-in.

### CLI-02 — Init templates

For each of `browser`, `react`, and `node`, initialize a new empty directory.

Expected:

- browser: `src/ffdb.ts` plus `.env.example`, BrowserSessionStore, no developer key;
- React: the browser files plus `src/FFDBProviders.tsx` and documented dependencies;
- Node: a server-only client using `FFDB_URL`, `FFDB_PROJECT_ID`, and
  `FFDB_DEVELOPER_KEY`, with an explicit secret warning;
- returned file and dependency lists are correct;
- no dependency is installed implicitly and no package file outside the target is
  changed;
- rerunning into an existing target refuses to overwrite the first conflicting
  file;
- an unknown template and filesystem-root target are rejected.

Compile each integrated fixture as covered in PKG-02 through PKG-04.

### CLI-03 — Type generation

With the manual schema active:

1. Run `ffdb generate --out <fixture>/ffdb.types.ts` and the alias
   `ffdb types generate --out <second-file>`.
2. Compare outputs byte-for-byte. Repeat without a schema change.
3. Confirm the header contains the schema version and the `Database` interface
   maps `documents` to a stable interface.
4. Confirm TEXT is `string`, INTEGER/REAL/date/timestamp wire values are `number`,
   BLOB uses imported `BlobValue`, nullable columns include `null`, and an
   unsupported declared type is conservatively `unknown`.
5. Add quoted identifiers, commas inside a default/check, table constraints, and
   colliding normalized table names in a temporary migration. Output must remain
   syntactically valid, deterministic, safely quoted, and collision-free.
6. Compile a fixture importing the generated interface. Generated types are aids;
   they must not claim to bypass server SQL parsing or RLS.

### CLI-04 — Administrative workflows and safety

Exercise health/dev, org/project list, schema, policies, SQL/file SQL, seed,
migration create/apply/status/rollback, API-key list/create/revoke, auth
settings/users/disable/enable, storage bucket/cleanup, email artifact/publish,
logs, backup list/create/restore/integrity.

Expected:

- commands map to the current `/v1` contract and project route;
- destructive rollback, member removal, key revocation, user disable, and restore
  prompt interactively; cancel leaves state unchanged;
- `--yes` enables intended automation but does not broaden scope;
- migration up/down/checksum behavior matches SETUP-06;
- malformed JSON/files/roles/scopes/limits fail before an unsafe request;
- output never prints a stored credential. A newly created API key is the only
  intentional one-time secret response and must be handled privately.

## Phase 7: persistence and restart

### PERSIST-01 — Record pre-restart state

Record safe fingerprints/counts for:

- organization/project IDs and active state;
- schema version, migration count, policy names, and Alice/Bob row counts;
- auth setting values and user active/verified state;
- bucket name, Alice-visible object key/size, and Bob-visible count zero;
- template kind/version/published time and Mailpit message count;
- audit-log count, backup UUID/status/size/hash prefix, and integrity result;
- organization usage summary, reporting status, and a safe hash/count inventory
  of files in the private metrics-data volume;
- latest sync schema version/control state, never the opaque cursor.

### PERSIST-02 — Retained-volume restart

```sh
make dev-down
make compose-rebuild
make status
```

Expected:

- Compose stops/recreates containers without deleting named volumes;
- readiness returns before functional verification begins;
- every PERSIST-01 control-plane, project SQLite, organization-metrics, MinIO,
  backup, and sync artifact remains consistent;
- Alice/Bob RLS isolation, object download, email template selection, backup
  integrity, and audit retrieval still pass;
- no duplicate platform migration, organization, project, API key, or application
  migration appears;
- portal reload retains tab-scoped selection/session as browser semantics allow;
  a clean tab/profile requires credentials again and never recovers a key from
  `localStorage`.

### PERSIST-03 — Crash/recovery spot checks

On the disposable environment only, restart the API during a safe GET and during
an idempotency-keyed backup/migration operation. Retry with the same key.
Expected: safe reads recover; keyed operations reconcile to one durable outcome;
worker receipts prevent duplicate application; unkeyed non-idempotent work is
not blindly replayed. Confirm project state does not remain permanently fenced.

## Phase 8: accessibility, responsive, and cross-browser

### A11Y-01 — Keyboard and focus

For landing, docs, and portal:

1. Start at the address bar and use only Tab, Shift+Tab, Enter, Space, arrows,
   Escape, and platform shortcuts.
2. Confirm a visible focus indicator on every interactive element, logical order,
   no trap outside a true modal/drawer, and no pointer-only action.
3. Test landing/docs skip links. In the portal, verify the first focus target and
   primary navigation provide an efficient path; record absence of a skip path if
   repeated navigation materially blocks keyboard users.
4. Open/close landing menu, docs drawer/search, portal Create menu, confirmation
   dialogs, password reveal, selects, file input, pagination, and toast dismissal.
5. On close, focus returns to a sensible trigger; background controls do not act
   while an `aria-modal` dialog is active.

### A11Y-02 — Screen reader and semantics

With one supported screen reader:

- confirm one useful page-level H1 and logical heading order;
- landmarks and navigation have useful names;
- labels, required state, error/alert/status, busy state, selected/current state,
  table headings, buttons, charts/images, and password visibility are announced;
- decorative SVG/canvas/animations remain hidden, while meaningful diagrams have
  useful alternatives;
- one-time-secret guidance is understandable without exposing it to evidence;
- dynamic query results, auth changes, errors, and toasts are announced without
  moving focus unexpectedly.

### A11Y-03 — Visual, contrast, zoom, and motion

1. Check normal text, large text, links, controls, focus rings, errors, disabled
   states, chart legends, light theme, and dark docs theme against WCAG AA.
2. At 200% and 400% zoom, complete sign-in, navigation, query, upload, and restore
   confirmation. Content may scroll in one dimension where semantically needed,
   but controls/text must not overlap or disappear.
3. Test Windows high contrast/forced colors where supported.
4. With reduced motion enabled, confirm decorative animation/transition/marquee/
   chart motion stops or is nonessential, and no task depends on animation.
5. Verify color is not the only indicator of active, error, success, RLS, or
   backup status.

### RESP-01 — Responsive layout

Test widths 320, 360, 390, 768, 1024, 1280, and 1440 pixels, plus portrait and
landscape on a real device.

Expected:

- no whole-page horizontal overflow at 320 pixels;
- the portal's navigation drawer and wide management tables scroll in their own
  containers without hiding the active item or route context;
- SQL Editor, Database, and Migrations use the available workspace height and
  width; schema, editors, results, history, and live rows remain
  resizable/reachable without stacked card padding consuming the viewport;
- forms stack, labels remain associated, and on-screen keyboards do not obscure
  the active input/submit action;
- long project names, IDs, request IDs, table SQL, object keys, email variables,
  and API-key prefixes wrap or truncate with an accessible way to understand the
  value;
- confirmations, toasts, menus, and search remain within the viewport;
- touch targets are usable without hover and adjacent destructive actions are
  not easily mis-tapped.

### BROWSER-01 — Cross-browser consistency

In every required browser:

- hard reload each app and open nested docs/project views;
- run developer and end-user sign-in, one RLS query, one storage upload/download,
  clipboard copy, file picker, confirmation, theme/session storage, Back/Forward,
  and sign-out;
- inspect console/network for CORS, CSP, mixed-content, module/export, source-map,
  unhandled promise, and layout errors;
- confirm locale dates and byte sizes remain understandable;
- verify Safari/WebKit handles session storage, signed S3 requests, Blob/File
  bodies, and new-window download safely.

## Cleanup and evidence closure

1. Restore auth settings changed during the plan.
2. Re-enable acceptance users needed for later investigation, or leave them
   explicitly disabled and documented.
3. Delete acceptance objects through authorized paths and abort multipart uploads;
   run bounded storage cleanup. There is no public bucket-delete promise, so do
   not bypass FFDB to remove metadata.
4. Revoke every acceptance API key, especially full-scope and `keys_rotate` keys.
   Confirm the active test process no longer uses them.
5. Sign out end-user and developer sessions. Run CLI logout and remove only the
   dedicated temporary config/scaffold/package fixtures.
6. Remove local browser site data for the acceptance origins after evidence is
   complete.
7. FFDB currently exposes no general organization/project deletion workflow.
   Leave uniquely labeled acceptance resources for operator retention or reset
   the entire **dedicated disposable** Compose environment under explicit
   operator approval.
8. `make clean` retains data and Docker volumes. Do not assume it erases test
   records. `docker compose down --volumes` is destructive and is permitted only
   after confirming the environment contains no required evidence or shared
   data.
9. Scan the evidence packet for bearer/API-key/action-token/signed-URL patterns,
   redact any occurrence, and record the scan method/result.
10. Attach the completed acceptance record, per-test outcomes, defect list,
    automated summaries, current-source/image proof, and final decision.

## Go/no-go

The candidate is a **go** only when all of the following are true:

- `make verify` and `make live` pass from the approved commit;
- Docker current-source/recreation proof is complete;
- bootstrap/sign-in, organization/project/key lifecycle, migration/schema/RLS,
  two-user isolation, auth/session security, storage, sync, email, audit, backup/
  restore/integrity, settings, SDK, CLI, and retained-volume restart pass;
- landing, docs, and portal pass the required desktop/mobile/browser matrix with
  no critical console, route, CORS, or build failure;
- no P0 or P1 defect is open;
- every P2 has an owner, safe workaround, scope statement, and explicit approval;
- accessibility-critical paths are keyboard and screen-reader operable and meet
  the approved contrast/reflow/reduced-motion baseline;
- contract/docs/package tarballs match the release and contain no test artifacts
  or embedded secrets;
- evidence is complete, reproducible, redacted, and approved by engineering,
  product, security, and operations owners appropriate to the release.

The candidate is a **no-go** if any authorization isolation result is ambiguous,
restore is unverified, a stale Docker image may have been tested, an advertised
web route cannot be exercised in the release deployment shape, a credential may
have leaked, or a required result is marked Blocked/N/A without release-owner and
security approval.
