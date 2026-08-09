# Information Architecture: FFDB Documentation

## Audience and task priority

The primary reader is an application developer or self-hosting operator using a packaged FFDB release. The most frequent path is install → configure → create a project → protect data → connect an SDK package. Contributors compiling the repository are a distinct audience served by the repository development runbook, not the public installation flow.

Every page follows the same minimum task contract: what the capability is, why it exists, when to use it, prerequisites, required values, a numbered workflow, an observable result, failure/recovery guidance, and next steps. Concept and reference pages use a review or decision workflow rather than pretending every task is a shell command.

## Site map

- Start here
  - Introduction `/`
  - Quickstart `/quickstart`
- Install
  - Docker Compose `/install/docker`
  - systemd `/install/systemd`
  - Deployment overview `/self-hosting`
  - Configuration `/configuration`
- Database
  - Architecture `/database`
  - Queries `/queries`
  - Migrations `/migrations`
  - Row-level security `/row-level-security`
  - SQL support `/sql-support`
- Auth and storage
  - Authentication `/authentication`
  - JWT claims `/jwt-claims`
  - Object storage `/storage`
  - Multipart uploads `/multipart-uploads`
- Sync and offline
  - Sync protocol `/sync`
  - Offline replicas `/offline`
  - Conflict behavior `/conflicts`
- SDKs and tools
  - TypeScript client `/client`
  - React `/react`
  - React Native `/react-native`
  - Sync client `/sync-client`
  - CLI `/cli`
- Billing and payments
  - FFDB platform billing `/billing/platform`
  - Project payments `/billing/project-payments`
- Operations
  - Backups and restore `/backups`
  - Observability `/observability`
  - Production security `/security`
- Reference
  - Client API `/reference/client`
  - Error envelopes `/reference/errors`
  - HTTP API `/reference/http-api`

## Navigation model

- Primary navigation: a single-level documentation sidebar grouped by user task. Group order follows the lifecycle from evaluation to reference.
- Secondary navigation: section anchors inside the current page and previous/next links across the flattened site map.
- Utility navigation: search, theme, install entry point, and portal access remain outside the content hierarchy.
- Mobile navigation: the same groups appear in a modal drawer; no route is desktop-only.
- Maximum depth: group → page → in-page section. New capabilities should extend a group before adding another navigation level.

## Content hierarchy

### Install pages

1. Distribution status and supported artifact — prevents unpublished channels from looking live.
2. Prerequisites and complete required configuration — makes hidden provider dependencies explicit.
3. Verifiable installation procedure — uses packaged artifacts, not repository builds.
4. Readiness, logs, upgrade, rollback, and data retention — defines the operator result and recovery path.

### Product capability pages

1. Capability boundary and correct use case.
2. Identity, schema, provider, or runtime prerequisites.
3. Current contract example.
4. Observable authorized result.
5. Stable failure modes and next adjacent task.

### Status pages

1. Explicit implemented, disabled, planned, or application-owned status.
2. Exact released endpoints/methods when they exist.
3. Safe behavior while unavailable.
4. No speculative command, price, or provider claim presented as live.

### Reference pages

1. Selection map for the released contract.
2. Required inputs and credential class.
3. Verification workflow against the matching deployed release.
4. Drift and compatibility failure behavior.

## Critical user flows

### Install and make the first query

1. Reader starts at Quickstart.
2. Reader obtains a checksum-verified `ffdb-compose-bundle-VERSION.tar.gz`.
   - Before public release: use a directly supplied bundle and local `file://` fixture.
   - After release notes enable the channel: use the documented curl bootstrap.
3. Reader configures `/etc/ffdb/ffdb.env` and starts the host with `ffdb-host`.
4. Reader bootstraps the first owner, creates an organization/project, and installs SDK-package tarballs.
5. Reader signs in an end user and verifies an RLS-scoped query.

### Choose an installation shape

1. Reader compares packaged Docker and advanced native systemd requirements.
2. If a complete single-host install is wanted, choose the pinned-image Compose bundle.
3. If every dependency and Linux service is operator-managed, choose the architecture-matched native component artifact.
4. Cargo crates and future Homebrew formulas remain component channels, not complete host installs.

### Add billing or payments

1. Reader distinguishes FFDB platform billing from payments inside an FFDB-backed project.
2. Reader checks the page's explicit current status and released contract.
3. If unavailable, no payment credential is collected and no speculative endpoint is called.
4. If implemented, the page names exact endpoints, idempotency, provider-disabled behavior, and SDK/CLI methods.

## Naming conventions

| Concept | Canonical label | Notes |
| --- | --- | --- |
| Complete Docker distribution | release bundle | `ffdb-compose-bundle-VERSION.tar.gz`; contains pinned images, configuration, installer, and `ffdb-host`. |
| JavaScript distribution | SDK package | Public `@ffdb/client`/`@ffdb/cli` npm package or verified GitHub Release `.tgz` integration; the channel must be explicit. |
| Native Linux distribution | native component artifact | Architecture-specific binary, web, and service files; advanced installation. |
| Repository build | contributor installation | Never the primary operator path. |
| Host lifecycle tool | `ffdb-host` | `install`, `start`, `stop`, `status`, `logs`, `upgrade`, `rollback`, `uninstall`. |
| FFDB service charging organizations | FFDB platform billing | Separate from application payments. |
| An application's own charges | Project payments | Provider integration owned by the application unless a released FFDB contract says otherwise. |
| Application identity | end-user session | Drives RLS scope; not a developer credential. |
| Trusted project administration | developer key | Never shipped to a browser bundle. |

## Component reuse map

| Component | Used on | Behavior |
| --- | --- | --- |
| Documentation shell | Every route | Sidebar, mobile drawer, search, theme, previous/next. |
| Task-depth sections | Every route | Injects the required purpose, prerequisites, workflow, result, failure, and next-step facets. |
| Code block | Contract and procedure pages | Language-aware highlighting, caption, keyboard focus, and copy status. |
| Callout | Security, availability, destructive work | Note or warning with a specific user action. |
| Search index | Every route | Includes visible copy, bullets, callouts, and code. |

## Content growth plan

- New routes must be added to navigation, the route guide map, search, and the depth test in one change.
- Billing pages transition from status to task documentation only when released endpoints and methods exist; status text remains explicit for provider-disabled behavior.
- SDK references grow by exported public methods, never by internal source modules.
- Install pages follow distribution contracts and release notes; contributor commands stay outside public install routes.
- Split a page only when its workflow, prerequisites, or failure ownership differs materially from the parent task.

## URL strategy

- URLs are stable, lowercase nouns with hyphens and no version segment.
- Install variants use `/install/<shape>`.
- Billing separates platform and project concerns under `/billing/*`.
- Compact lookup pages use `/reference/*`.
- The deployed docs mount strips `/docs` before route lookup and tolerates one trailing slash.
- Search does not create indexable query-string routes; it resolves to existing pages.
