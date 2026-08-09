# FFDB portal production information architecture

This document is the interaction contract for the FFDB portal. It supersedes
prototype-only behavior described in earlier visual-fidelity notes. The original
FFDB component language remains the visual baseline; live platform behavior and
truthful data take precedence over decorative prototype content.

## Users and scopes

The shell always makes the active instance, organization, and project visible.
Instance switching changes the authenticated control plane. Organization and
project switching change the working scope without implying that either is
healthy or configured.

- Instance owner: setup, instance policy, administrators, plans, provider
  configuration, organizations, users, and every organization/project route.
- Instance administrator: the same operational views allowed by the server role,
  without owner-only controls.
- Organization administrator: projects, members, billing, and project routes in
  organizations they administer.
- Developer: project build and operate routes allowed by their project role.
- Viewer: read-only views with create, mutate, rotate, and destructive controls
  omitted or disabled with an explanation.
- Project end user: not a portal role. End-user Auth and Sync sessions are a
  separate test context inside the active project and never replace the signed-in
  developer session.

## Navigation and route ownership

| Group | Route | Primary job | Required scope |
| --- | --- | --- | --- |
| Workspace | Overview | Understand project state and continue work | Project |
| Workspace | Projects | Create, switch, and manage projects | Organization |
| Workspace | Members | Invite people and manage organization roles | Organization |
| Build | SQL Editor | Compose and run ad-hoc SQL, inspect results | Project |
| Build | Database | Inspect schemas, tables, rows, and migration history | Project |
| Build | Migrations | Author, apply, inspect, and roll back migrations | Project |
| Build | Policies | Inspect and edit RLS policies | Project |
| Build | Auth | Configure auth, inspect users, manage a test end-user session | Project |
| Build | Storage | Create buckets and inspect objects | Project |
| Build | Sync | Fetch snapshots and changes with the test end-user session | Project |
| Build | Email | Inspect, edit, validate, and publish templates | Project |
| Operate | Activity | Search, filter, sort, and page audit events | Project |
| Operate | Backups | Create backups and inspect restore-test state | Project |
| Operate | Usage | Understand organization metering, plan, and invoices | Organization |
| Sell | Products | Configure the project payment account, products, and prices | Project |
| Sell | Orders | Inspect payments/orders and update fulfillment | Project |
| Sell | Subscriptions | Inspect subscriptions and entitlements | Project |
| Administration | Instance | Configure and operate the installation | Instance administrator |
| Administration | Instance Billing | Configure provider and tenant plan catalog | Instance administrator |
| Administration | Instance Users | Manage platform users and administrators | Instance administrator |
| Administration | Settings | Manage the active portal connection and project keys | Signed-in developer |
| Account | Account | Identity, sessions, and saved instance connections | Signed-in developer |

## Action contract

Every visible action must satisfy exactly one of these outcomes:

1. Navigate to a working route and preserve the relevant scope or draft.
2. Open a labeled form/dialog whose submission calls a real API and reports the
   resulting entity or server error.
3. Change an in-page data view (filter, sort, pagination, tab, selection) with an
   accessible state.
4. Perform a real API operation with pending, success, and failure feedback.

A toast by itself is never the outcome of a navigation or row-selection action.
Rows are not clickable unless they open a detail surface or change an explicit
filter. Expected prerequisites are presented before a request is sent; for
example, Sync actions remain disabled until a project end-user session exists.

The global Create menu maps to distinct workflows:

- New project → Projects, with the create-project form opened.
- New migration → Migrations, with a new migration draft opened.
- Storage bucket → Storage, with the create-bucket form opened.
- Invite member → Members, with the invite form opened.

## List and table contract

Unbounded or operational lists expose search, relevant categorical filters,
sortable columns, a result count, page-size selection, previous/next controls,
and an unambiguous empty state. The first release may page already-fetched server
results client-side where no paged endpoint exists, but must not imply that the
result set is exhaustive when the API reports a limit.

At narrow widths, tables either retain essential columns in a horizontally
scrollable region with a visible edge or render rows as labeled records. Page
controls never cause document-level horizontal overflow.

## Responsive contract

- 1280 px and wider: persistent sidebar, fluid content, at most three balanced
  summary cards or a 2:1 work/detail split.
- 768–1279 px: persistent or drawer navigation depending on available width;
  two-column summary grids collapse before controls clip.
- 480–767 px: drawer navigation; single-column work surfaces; toolbars wrap;
  dialogs and editors fit the viewport.
- 320–479 px: no document-level horizontal overflow; primary actions remain
  reachable; labels may shorten but active instance/project context remains
  available.

The acceptance viewports are 1440×900, 1024×768, 768×1024, and 390×844.

## State contract

All routes have deliberate loading, empty, ready, unauthorized, degraded, and
error states where applicable. Empty is not an error. Missing project end-user
authentication is not a Sync request failure. Unavailable metrics are labeled
unavailable and are never rendered as zero, healthy, or operational. Destructive
actions require confirmation and refresh the affected resource after success.
