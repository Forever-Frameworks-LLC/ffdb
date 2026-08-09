# Atlas overview design specification

Source: `atlas-overview-concept.png` (1536 × 1024). The concept is the visual
contract for the initial portal overview screen.

## Visible-copy lock

The first viewport contains `FFDB`, `Northstar Labs`, `Atlas`, the sidebar items
`Overview`, `SQL Editor`, `Database`, `Policies`, `Auth`, `Storage`, `Sync`,
`Email`, `Logs`, `Backups`, and `Settings`; the top-bar labels `Atlas`,
`SQLite database`, `Healthy`, `Docs`, `CLI`, and `Create`; the title
`Atlas overview`; and the helper `One project. One SQLite database.`. No eyebrow,
environment selector, marketing copy, or decorative badges may be added.

## Layout and container model

- Fixed 228 px ink sidebar and a fluid near-white application canvas.
- 60 px top project bar. Main content uses an 18 px gutter and a twelve-column
  alignment grid.
- The title consumes 96 px vertically. The health rail spans nine columns while
  usage spans three. Sync activity and recent activity use the same nine-column
  span; quick actions and worker status occupy the right rail.
- Regions are squared panels with 4 px radii and cool-gray 1 px rules. There are
  no nested cards, floating glass surfaces, gradients, or decorative shadows.
- At 900–1199 px the right rail moves beneath the main regions. Below 760 px the
  sidebar becomes a horizontal, scrollable navigation band and all regions use a
  single column.

## Tokens

| Role | Value |
| --- | --- |
| canvas | `#f7f8fa` (cool near-white; never cream) |
| surface | `#ffffff` |
| sidebar | `#031425` |
| sidebar selected | `#0a243d` |
| text | `#101828` |
| muted | `#4b5565` |
| faint | `#7c8798` |
| rule | `#d6dce5` |
| primary | `#0868e8` |
| healthy | `#0ba45b` |
| attention | `#ed8206` |
| radius | `4px` panels, `5px` controls, circular status/avatar |
| shadow | none except a subtle focus ring |
| spacing | `4, 8, 12, 16, 20, 24, 32px` |
| motion | `140ms` state changes; respect reduced motion |

Typography uses Inter when available, then a system neo-grotesk stack. The page
title is 42/46 at 650 weight on the native canvas; panel titles are 18/24 at 650;
controls and table cells are 13–14 px; metadata is 12 px. Numeric data uses
tabular figures.

## Component and icon inventory

- `AppShell`, `Sidebar`, `ProjectBar`, `PageHeader`, `Panel`, `HealthRail`,
  `UsageSummary`, `SyncChart`, `ActivityTable`, `QuickActions`, `WorkerStatus`.
- One consistent 20 px outline icon family with 1.6 px rounded strokes. Icons:
  home, terminal/code, database, shield, users, archive/storage, sync, mail, list,
  cloud-backup, settings, book, bell, plus, chevrons, external link, and resource
  icons. Status indicators use filled circles/checks.
- Buttons have deliberate 14 px/20 control type. The solid cobalt primary button
  is reserved for `Create` and the currently emphasized quick action.
- Selected navigation uses a cobalt 4 px left marker plus the darker ink row;
  hover and keyboard focus are distinct.

## Data and interaction inventory

- Health groups: API, Database worker, PostgreSQL, Object storage, Email delivery.
  Each has an operational status, a small sparkline, and two compact values.
- Usage rows: Database `384 MB / 1 GB`, Storage `1.8 GB / 5 GB`, Egress `6.4 GB`,
  Requests `142k`.
- Sync chart toggles Pull, Push, and Resnapshot. Toggling a legend item changes
  the actual plotted series and its accessible pressed state.
- Activity rows are clickable and keyboard-focusable. Pagination changes the
  current page label. `View all logs` is a real link affordance.
- Quick actions update an in-page command status. The project selector and
  create menu expose local menus; notification and profile controls are usable.
- Worker status shows `ffdb-worker-01`, four active projects, p95 82 ms, CPU 37%,
  and memory 41%.

## Media treatment

There is no raster media inside the app and no color overlay. The concept itself
is retained only as a design reference. Charts, icons, labels, and controls are
code-native SVG/HTML so they remain interactive, responsive, and accessible.
