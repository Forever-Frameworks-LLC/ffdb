# Documentation source inventory and implementation spec

## Reference and intent

The read-only reference is `private-code/docs`, a Tailwind Plus Protocol-style documentation shell. The implementation keeps that information-dense shell while replacing the old hosted-product information architecture with the current repository architecture, SDK names, CLI grammar, storage model, and security boundaries.

## Visual system and shell

- Neutral white/zinc canvas with an emerald active marker and syntax accents; dark mode uses zinc-950 surfaces.
- Persistent 288–320px left rail on desktop with compact expandable navigation groups.
- Fixed 56px translucent header with search, top-level links, theme toggle, and portal action.
- Main prose column is 720–760px with generous top space, 28px line-height, restrained 1px dividers, bordered callouts, and dark rounded code blocks.
- Mobile uses a fixed header and slide-in navigation drawer. Search is a keyboard-friendly command-style overlay.
- Page title, lead, section headings, callouts, properties, code examples, previous/next links, and footer form the reusable content vocabulary.

## Source information architecture

- Start here: Introduction, Quickstart, Create an app, Generate types.
- Core concepts: Client, Configuration, Authentication, Database queries, Access control, File storage.
- Offline-first: Overview, Local SQLite cache, Sync lifecycle, Mutation queue, Conflict behavior.
- Frameworks: React, Next.js, React Native/Expo, Node.js, Electron.
- Reference: createClient, React hooks, storage adapters, files API, network adapters, errors.

## Updated FFDB information architecture

- Start here: Introduction, Quickstart.
- Install: Docker Compose, systemd, Deployment overview, Configuration.
- Database: Architecture, Queries, Migrations, Row-level security, SQL support.
- Auth and storage: Authentication, JWT claims, Object storage, Multipart uploads.
- Sync and offline: Sync protocol, Offline replicas, Conflict behavior.
- SDKs and tools: TypeScript client, React, React Native, Sync client, CLI.
- Billing and payments: FFDB platform billing, Project payments.
- Operations: Backups and restore, Observability, Production security.
- Reference: Client API, Error envelopes, HTTP API.

Every navigation target must resolve to local content. Every page answers what, why, when, prerequisites, required values, procedure, expected result, failure/recovery, and next steps. Search indexes page titles, descriptions, visible section copy, callouts, bullet lists, and code. Previous/next navigation follows the flattened IA.

## Content truth constraints

- FFDB is self-hostable: a PostgreSQL control plane plus one hardened SQLite application database per project, executed by isolated Rust workers.
- The complete host distribution is `ffdb-compose-bundle-VERSION.tar.gz`, installed under `/opt/ffdb` with configuration under `/etc/ffdb` and operated through `ffdb-host`. Its explicit `single-host` evaluation profile includes digest-pinned PostgreSQL, MinIO, persistent Mailpit, FFDB services, and the gateway; the external-provider profile remains the internet-production path.
- Native systemd installation uses separately verified `ffdb-native-linux-ARCH-VERSION.tar.gz` component artifacts. Cargo crates and the future Homebrew controller formula are component channels, not complete server installation paths.
- All public npm packages use the `@ffdb` scope: `@ffdb/client`, `@ffdb/cli` (binary: `ffdb`), `@ffdb/react`, `@ffdb/react-native`, `@ffdb/sync-client`, and `@ffdb/email-components`. Matching checksum-listed `.tgz` assets remain available for verified offline installation; workspace links are contributor-only.
- The TypeScript entry point is `new FFDBClient({ baseUrl, projectId, developerKey? })`; queries use tagged parameters and return ordered row arrays.
- End-user requests use built-in auth sessions; developer keys are separate, scoped administration credentials.
- RLS accepts a documented PostgreSQL-style policy DDL subset and compiles enforcement into protected SQLite views/triggers. Unsupported forms fail closed.
- Sync is logical snapshot/push/pull with opaque scope-bound cursors, server-sequence last-write-wins, tombstones, and explicit resnapshot controls—not WAL replication or CRDT.
- Storage bytes live in an S3-compatible provider; metadata, quotas, versions, multipart state, and authorization live behind project RLS. Signed URLs are short-lived capabilities.
- Docker deployment examples use the packaged pinned-image Compose bundle, `ffdb-host`, and `/readyz`; they do not require a source checkout. Native deployment examples use the matching release's API, database-worker, sync-worker, web, environment, sysusers, tmpfiles, and single-process Caddy artifacts.
- FFDB platform billing and Project payments are separate information-architecture concepts. Each page must state implemented/disabled/planned status and may describe endpoints only after they exist in the current release contract.
- Do not claim managed hosting, free tiers, fixed pricing, formal certifications, universal framework support, guaranteed latency, or uptime.

## Responsive and accessibility requirements

- Desktop sidebar becomes an accessible modal drawer below 1024px.
- Search opens from a labeled control and Cmd/Ctrl+K, focuses the input, closes on Escape, and exposes result links.
- Theme preference persists locally and defaults to the system preference.
- Active navigation uses both color and `aria-current`; expandable groups use `aria-expanded`.
- Code blocks use deterministic language-aware syntax highlighting, remain horizontally scrollable and keyboard focusable, expose captions, and give copy buttons meaningful labels and status feedback. Focus rings are always visible, and reduced motion is respected.

## 2026-08-03 browser verification

- Desktop 1280 × 720: verified the persistent dark navigation rail, fixed header, focused prose column, on-page outline, callouts, and syntax-highlighted shell/TypeScript examples against `source-desktop.png`.
- Mobile 390 × 844: verified the compact header, hidden drawer state, single-column prose, readable hierarchy, and horizontally scrollable code in `output/playwright/docs-quickstart-mobile-390x844.png` and `output/playwright/docs-install-mobile-390x844.png`.
- The information architecture contains 31 routes in nine groups. Each route is regression-tested for what/why/when, prerequisites and values, workflow, expected result, failures/recovery, and next steps.
- Copy intentionally replaces the prototype's hosted-app quickstart with the signed release installer, production topology split, current SDK/CLI contracts, and honest billing/payment status.
