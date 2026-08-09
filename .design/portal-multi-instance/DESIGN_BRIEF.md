# Design Brief: FFDB Multi-Instance Portal and First-Run Setup

## Objective

Make a fresh FFDB installation understandable and operable without repository knowledge or hidden provider prerequisites. The first owner must complete instance configuration, then create the first organization and project before entering the normal portal. After setup, every screen must make the active instance, organization, project, account role, and billing context obvious.

## Audiences

- Self-hosted owners operating a private or team installation.
- Platform operators offering managed Free, Usage, or Pro access.
- Organization owners and administrators managing membership and billing.
- Project administrators and developers managing data, auth, sync, storage, commerce, and operations.
- Ordinary organization/project members who must not see instance-administration controls.

## Ranked User Needs

1. Know which instance, organization, and project is active before taking an action.
2. Switch those scopes without signing out or manually editing identifiers.
3. Complete a fresh installation entirely through guided onboarding.
4. Create and manage organizations, projects, users, roles, billing, and account settings from focused pages.
5. Understand whether the account is self-hosted or managed and whether its plan is Private, Team, Free, Usage, or Pro.
6. Find documentation and operational help from the relevant task.

## Critical First-Run Flow

1. Verify the one-time bootstrap token and create the first owner.
2. Choose Private, Team, Managed Service, or Platform operation.
3. For a billable platform, choose operator-owned Stripe credentials or Stripe Connect and enter every required value in onboarding.
4. Create the first organization.
5. Create the first project and make it active.
6. Enter the project overview.

Organizations and projects remain unavailable until steps 1–3 are complete. The portal must explain the gate instead of exposing disabled or misleading creation controls.

## Product Structure

- Navigation depth: at most three explicit scopes—instance, organization, project—followed by one task page.
- Primary working view: project overview and project task pages.
- Growing content: instances, organizations, projects, users, activity, usage, orders, subscriptions, backups, and documentation references.
- Fixed content: account profile, instance mode, provider configuration, legal pages, and core navigation categories.

## Required Navigation Groups

- Workspace: Overview, Projects, Members.
- Build: Database, Policies, Auth, Storage, Sync, Email.
- Operate: Activity, Backups, Usage.
- Sell: Products, Orders, Subscriptions.
- Administration: Instance, Billing, Users, Settings.
- Utility: Docs, account/profile, sign out, create menu.

Navigation is role-aware. Controls a user cannot access are omitted unless their absence would hide an upgrade or request-access path; they are not rendered as unexplained disabled items.

## Visual Direction

Preserve the existing FFDB Atlas visual system: navy application rail, white surfaces, blue primary action, green health states, fine borders, square geometry, restrained radii, Inter-like UI typography, tables and rails instead of a consumer card grid, and no gradients or decorative illustration.

Accepted references:

- `apps/portal/design/multi-instance-shell-concept.png`
- `apps/portal/design/first-run-onboarding-concept.png`
- `apps/portal/design/multi-instance-mobile-concept.png`

## Responsive Behavior

- Desktop uses a persistent navigation rail with three stacked scope switchers.
- Mobile uses a compact top bar, three explicit scope rows, and a menu drawer.
- Tables become scroll-safe lists or focused detail views; the document itself must not overflow horizontally.
- The active scope and primary action remain visible without a ten-item horizontal icon strip.

## Functional Constraints

- Use real FFDB API operations and types; do not invent successful actions.
- Provider secrets are submitted to the API, encrypted at rest, and never returned.
- Stripe Connect uses Accounts v2 responsibility/capability semantics.
- Project and organization creation are server-gated until setup completion, not merely hidden by the portal.
- Local verification defaults to the current page origin (`http://127.0.0.1:5173` in development) and supports an explicit API/public-origin override without requiring the production domain.
