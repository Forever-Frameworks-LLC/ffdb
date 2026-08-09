# Information Architecture: FFDB Portal

## Site Map

- Access `/app/access`
  - Bootstrap owner `/app/setup/owner`
  - Instance type `/app/setup/instance`
  - Payments `/app/setup/payments`
  - First workspace `/app/setup/workspace`
- Project overview `/app/projects/:projectId/overview`
- Workspace
  - Projects `/app/organizations/:organizationId/projects`
  - Members `/app/organizations/:organizationId/members`
- Build
  - SQL editor `/app/projects/:projectId/sql`
  - Database `/app/projects/:projectId/database`
  - Policies `/app/projects/:projectId/policies`
  - Auth `/app/projects/:projectId/auth`
  - Storage `/app/projects/:projectId/storage`
  - Sync `/app/projects/:projectId/sync`
  - Email `/app/projects/:projectId/email`
- Operate
  - Activity `/app/projects/:projectId/activity`
  - Backups `/app/projects/:projectId/backups`
  - Usage `/app/organizations/:organizationId/usage`
- Sell
  - Products `/app/projects/:projectId/products`
  - Orders `/app/projects/:projectId/orders`
  - Subscriptions `/app/projects/:projectId/subscriptions`
- Administration
  - Instance overview `/app/instance`
  - Instance billing `/app/instance/billing`
  - Platform users `/app/instance/users`
  - Instance settings `/app/instance/settings`
- Account `/app/account`

The Vite SPA receives every `/app/*` path from the gateway. Client-side navigation writes these paths with the History API and restores the active page on reload.

## Navigation Model

- **Primary navigation:** A persistent desktop rail grouped as Workspace, Build, Operate, Sell, and Administration. Only role-relevant groups appear.
- **Scope navigation:** Instance, organization, and project selectors appear above task navigation. Each selector has its own label and never combines IDs from different levels.
- **Secondary navigation:** Focused tabs exist only within a task when the API surface is too dense for one screen, such as database schema/migrations or instance provider/catalog.
- **Utility navigation:** Documentation, create menu, account/profile, plan/mode, and sign out.
- **Mobile navigation:** A compact top bar opens the grouped navigation drawer. Three full-width scope rows remain visible above page content. The active task title and create action follow.

## Content Hierarchy

### Project Overview

1. Project identity, endpoint, and health—prevents cross-project actions.
2. Service health and usage—answers whether the project is operational and within plan.
3. Recent activity—supports audit and troubleshooting.
4. Quick actions—starts common work within the visible project context.

### Workspace Projects

1. Active organization and create-project action.
2. Searchable project list with state, plan, and last activity.
3. Project selection and credentials as explicit follow-up actions.

### Workspace Members

1. Organization member list and roles.
2. Invitation flow.
3. Role and removal controls with destructive confirmation.

### Instance Administration

1. Deployment mode, setup state, and provider health.
2. Instance administrators and organization policy.
3. Separate organization, user, billing-plan, and provider task pages.

### Account

1. Signed-in identity, active instance, organization, project, and role.
2. Managed/self-hosted and plan context.
3. Session controls and saved instance connections.

## User Flows

### Fresh Installation

1. The deployment exposes the portal at its configured public/local origin.
2. The first visitor enters the one-time bootstrap token and creates the owner.
3. The owner chooses Private, Team, Managed Service, or Platform.
4. For Platform, the owner chooses BYO Stripe or Connect and enters every required value.
5. The server marks setup complete only after provider configuration is valid or no provider is required.
6. The owner creates the first organization and project.
7. The new project becomes active and the portal opens its overview.

### Switch Project

1. User opens the Project selector.
2. Portal lists projects within the active organization.
3. User selects a project.
4. Client context and URL update together; the next operation uses the selected project ID.

### Switch Instance

1. User opens the Instance selector.
2. Portal lists locally saved instance connections by friendly name and origin.
3. User selects an instance.
4. Portal creates an isolated session/client namespace for that origin and re-checks access/setup state.

### Manage Users

1. Authorized user opens Members for the active organization or Users for the instance.
2. Portal displays the correct scope and permissions.
3. User invites, changes role, enables/disables, or removes an account.
4. The API authorizes and audits the mutation; the portal reloads the focused list.

## Naming Conventions

| Concept | Label in UI | Notes |
|---|---|---|
| FFDB server/deployment | Instance | One API/control-plane origin. |
| Tenant boundary | Organization | Owns projects, members, plan, and usage. |
| Application boundary | Project | Owns one SQLite database and project services. |
| Platform charging model | Instance billing | What organizations pay the FFDB operator. |
| Application commerce | Project commerce | Products, orders, payments, and subscriptions inside one project. |
| Non-billable self host | Private / Team | Usage remains visible; tenant charges are disabled. |
| Operator-hosted plans | Free / Usage / Pro | Shown with the managed instance context. |

## Component Reuse Map

| Component | Used on | Behavior differences |
|---|---|---|
| `PortalShell` | Every authenticated page | Desktop rail vs mobile drawer. |
| `ScopeSelector` | Instance, organization, project | Source list and selection callback vary. |
| `PageHeader` | Every task page | Breadcrumb, description, and actions vary. |
| `ManagementTable` | Projects, members, users, orders, activity | Columns and row actions vary. |
| `ResourceState` | All API-backed pages | Loading, empty, error, and retry content vary. |
| `SetupProgress` | First-run steps | Active and completed step state. |
| `AccountContext` | Sidebar/footer and account page | Compact vs expanded presentation. |

## Content Growth Plan

- Instances are stored locally and remain a short searchable list.
- Organizations/projects/users/activity/orders/subscriptions use paginated API collections.
- Documentation links point to stable task routes rather than repository files.
- New project capabilities join Build, Operate, or Sell instead of expanding one flat navigation list.
- New platform administration capabilities join a focused Administration page rather than the project settings page.

## URL Strategy

- Task pattern: `/app/<scope>/<scope-id>/<task>`.
- Dynamic segments are stable opaque organization/project IDs, not display names.
- `/app/instance/*` is reserved for instance-owner/administrator work.
- Query parameters are limited to filters, pagination, provider return state, and create dialogs.
- The active URL is authoritative for task restoration; saved scope is used only when a URL omits a required identifier.
