# Information Architecture: FFDB public site and documentation

## Assumed structural decisions

- Primary tasks, in order: choose a deployment path; start FFDB; create and protect a project database; connect an application; add offline sync; operate the deployment.
- Navigation depth: two levels. The sidebar group is level one and a page is level two; headings remain on-page anchors.
- Growth: SDK, deployment, and operations content will grow. Keep these as stable groups rather than adding one-off top-level pages.
- Reader entry points: operators enter through Installation/Operations; application developers enter through Quickstart/SDKs; evaluators enter through the landing deployment section.
- Highest-use pages: Quickstart during evaluation, then TypeScript client and the selected deployment guide during ongoing development/operation.

## Site Map

- Landing `/`
  - Capabilities `/#capabilities`
  - Architecture `/#architecture`
  - Security `/#security`
  - Deployment `/#deployment`
- Documentation `/docs/`
  - Start here
    - Introduction `/docs/`
    - Choose a deployment `/docs/self-hosting`
    - Docker Compose `/docs/install/docker`
    - systemd `/docs/install/systemd`
    - Application quickstart `/docs/quickstart`
    - Configuration `/docs/configuration`
  - Database
    - Architecture `/docs/database`
    - Queries and transactions `/docs/queries`
    - Migrations `/docs/migrations`
    - Row-level security `/docs/row-level-security`
    - SQL support `/docs/sql-support`
  - Auth and storage
    - Authentication `/docs/authentication`
    - JWT claims `/docs/jwt-claims`
    - Object storage `/docs/storage`
    - Multipart uploads `/docs/multipart-uploads`
  - Sync and offline
    - How sync works `/docs/sync`
    - Offline replicas `/docs/offline`
    - Conflict behavior `/docs/conflicts`
  - SDKs and tools
    - Runtime support matrix `/docs/client`
    - React `/docs/react`
    - React Native `/docs/react-native`
    - Sync client `/docs/sync-client`
    - CLI `/docs/cli`
  - Operations
    - Backups and restore `/docs/backups`
    - Observability `/docs/observability`
    - Production security `/docs/security`
  - Reference
    - Client API `/docs/reference/client`
    - Error envelopes `/docs/reference/errors`
    - HTTP API `/docs/reference/http-api`
- Portal `/app/`

## Navigation Model

- **Primary navigation:** Landing links to Capabilities, Architecture, Security, Documentation, Portal, and Run locally. The docs header links back to FFDB and the portal.
- **Secondary navigation:** A grouped desktop sidebar lists every docs page. Start here begins with deployment choice and exposes Docker/systemd directly rather than hiding them behind Self-hosting.
- **Utility navigation:** Search, theme, API reference, and portal access remain in docs chrome.
- **Mobile navigation:** The same groups appear in a drawer. Search remains a separate labeled control and every route is reachable without opening an on-page table of contents.

## Content Hierarchy

### Landing

1. Outcome: add a backend while retaining control of application data.
2. Proof: project isolation, RLS, offline sync, and protected storage.
3. How it works: install, protect data, connect an app.
4. Runtime/package support and honest distribution status.
5. Deployment choices: open self-hosting now, managed option later.

### Installation pages

1. Who the path is for and what it installs.
2. Prerequisites and concrete commands.
3. Configuration/secrets and filesystem ownership.
4. Health verification and first project.
5. Upgrade, rollback, logs, and removal cautions.

### SDK and sync pages

1. Runtime support matrix and package responsibility.
2. Complete construction example.
3. Persistence/session adapter requirements.
4. Main workflow and observable state.
5. Failure, retry, resnapshot, and security behavior.

### Feature pages

1. User problem and expected outcome.
2. Current supported contract.
3. End-to-end example.
4. Limits and security implications.
5. Verification/troubleshooting link.

## User Flows

### Self-host with Docker

1. User sees Docker and systemd on the docs introduction and sidebar.
2. User opens Docker Compose and checks prerequisites.
3. User configures local or production environment values.
4. User starts current-source images and verifies health.
5. User opens the portal, bootstraps/signs in, and creates a project.
6. User continues to the application quickstart.

### Install as Linux services

1. User chooses systemd from deployment comparison.
2. User builds release binaries and creates the service account/directories.
3. User installs environment and unit files with least privilege.
4. User starts PostgreSQL/object storage dependencies, API, and sync worker.
5. User verifies readiness, logs, restart behavior, and upgrade/rollback steps.

### Add offline sync

1. Developer chooses their runtime in the support matrix.
2. Developer constructs `FFDBClient` with that runtime's fetch/session store.
3. Developer supplies `OfflineSyncClient` with a persistent replica adapter: native SQLite on React Native, or an application-provided adapter in browser/Node.
4. App queues a mutation locally and invokes sync on connectivity/app lifecycle events.
5. UI subscribes through `useSync` or `subscribe`, handles rejected mutations, and resnapshots when instructed.

## Naming Conventions

| Concept | Label in UI | Notes |
| --- | --- | --- |
| Open product | FFDB or Community source | Apache-2.0 source available now |
| Operated future product | Managed FFDB | Always append Planned/not available yet |
| Infrastructure setup | Self-hosting | Parent concept for Docker and systemd |
| Browser/server HTTP package | TypeScript client | `@ffdb/client` |
| Synchronization engine | Sync client | `@ffdb/sync-client`; does not itself persist data |
| Local synchronized store | Replica adapter | Runtime-specific transactional persistence contract |
| Project application credential | End-user session | Safe for application runtime after sign-in |
| Operator credential | Developer session/key | Never embed in browser/native apps |

## Component Reuse Map

| Component | Used on | Behavior differences |
| --- | --- | --- |
| Docs shell | Every docs route | Desktop sidebar, mobile drawer |
| Code block | Install, SDK, feature, reference pages | Language-specific highlighting and copy state |
| Callout | Every docs group | Note and warning semantics |
| Runtime matrix | Client and sync pages | HTTP/session/replica requirements by runtime |
| Deployment comparison | Landing and self-hosting | Compact marketing summary vs operational detail |
| Previous/next pager | Every docs route | Follows sidebar information order |

## Content Growth Plan

- Add new deployment targets beneath Start here only when supported artifacts and verification steps ship.
- Add framework bindings beneath SDKs and tools; keep the runtime matrix authoritative.
- Add operational runbooks beneath Operations and link them from feature cautions.
- Search indexes titles, descriptions, headings, paragraphs, and code labels. Keep route slugs stable and redirect any future rename.

## URL Strategy

- Pattern: `/docs/<topic>` for product concepts and `/docs/install/<method>` or `/docs/reference/<surface>` for scoped subtrees.
- Dynamic segments: none in the static public docs.
- Query parameters: reserved for future search/filter state; page identity remains path-based.
