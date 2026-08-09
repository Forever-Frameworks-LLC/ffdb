# Information Architecture: FFDB distribution and billing

## Assumed structural decisions

- Most frequent operator task: install or upgrade one release bundle, then inspect health and logs.
- Most frequent developer task: install an SDK package, connect an application, and follow a feature recipe.
- Navigation remains two levels deep. Each page owns detailed headings rather than creating a third sidebar level.
- Installation, SDKs, billing, project payments, and operations will grow; each receives a stable group or page family.
- The portal overview remains the primary management surface and gains organization billing/project payment entry points only when corresponding APIs are available.

## Site map

- Landing `/`
  - Install `/#install`
  - Capabilities `/#capabilities`
  - Architecture `/#architecture`
  - Security `/#security`
  - Billing `/#billing`
- Documentation `/docs/`
  - Start here
    - Introduction `/docs/`
    - Install FFDB `/docs/install`
    - Configure `/docs/configuration`
    - Create the first project `/docs/quickstart`
  - Installation
    - Release bundle `/docs/install/release-bundle`
    - Docker Compose `/docs/install/docker`
    - Linux systemd `/docs/install/systemd`
    - SDK and CLI packages `/docs/install/packages`
    - Contributor source setup `/docs/contributing/source-install`
  - Database
    - Architecture `/docs/database`
    - Queries and transactions `/docs/queries`
    - Migrations `/docs/migrations`
    - Row-level security `/docs/row-level-security`
    - SQL support `/docs/sql-support`
  - Auth, files, and email
    - Authentication `/docs/authentication`
    - JWT claims `/docs/jwt-claims`
    - Object storage `/docs/storage`
    - Multipart uploads `/docs/multipart-uploads`
    - Transactional email `/docs/email`
  - Sync and offline
    - Protocol `/docs/sync`
    - Offline replicas `/docs/offline`
    - Conflicts `/docs/conflicts`
  - Billing and payments
    - Choose the billing domain `/docs/billing`
    - FFDB platform plans `/docs/billing/platform`
    - Project checkout and subscriptions `/docs/payments/checkout`
    - Webhooks and fulfillment `/docs/payments/webhooks`
    - Marketplace accounts `/docs/payments/connect`
  - SDKs and tools
    - TypeScript client `/docs/client`
    - React `/docs/react`
    - React Native `/docs/react-native`
    - Sync client `/docs/sync-client`
    - CLI `/docs/cli`
  - Operations
    - Upgrades and rollback `/docs/upgrades`
    - Backup and restore `/docs/backups`
    - Observability `/docs/observability`
    - Production security `/docs/security`
  - Reference
    - Configuration values `/docs/reference/configuration`
    - Client API `/docs/reference/client`
    - Error envelopes `/docs/reference/errors`
    - HTTP API `/docs/reference/http-api`
- Portal `/app/`
  - Organization billing `Billing`
  - Project payments `Payments`

## Navigation model

- **Landing:** primary install action, documentation, portal, and billing explanation. Repository commands appear only in contributor content.
- **Docs desktop:** grouped sidebar with Installation and Billing and payments visible without search.
- **Docs mobile:** the same groups in a drawer; current group expands automatically.
- **Portal:** Billing is organization-scoped; Payments is project-scoped. Labels never collapse both into “Billing.”

## Content hierarchy

### Install page

1. Supported release channel and exact artifact installed.
2. Host, DNS, TLS, PostgreSQL, S3, and email prerequisites.
3. One command plus a transparent manual-bundle equivalent.
4. Required/optional configuration table with sources and examples.
5. Start, bootstrap, health, logs, upgrade, rollback, and removal.

### Feature page

1. Problem solved and when to use it.
2. Trust/data model and prerequisites.
3. Complete request or application example.
4. Inputs, outputs, limits, failure modes, and security notes.
5. Verification and next related task.

### Platform billing page

1. Difference from project payments.
2. Free allowance and entitlement model.
3. Provider configuration and price mapping.
4. Checkout, webhook, entitlement, portal, and cancellation lifecycle.
5. Enforcement, recovery, audit, and disabled-billing behavior.

### Project payments page

1. Project-scoped provider boundary.
2. One-time and subscription checkout contracts.
3. Customer/user/org membership mapping.
4. Webhook verification and fulfillment transaction.
5. Refund, dispute, cancellation, replay, and observability behavior.

## Critical user flows

### Install without source

1. Operator chooses a release version or stable channel.
2. Installer downloads and verifies the release bundle.
3. Operator fills generated owner-only configuration.
4. Lifecycle command starts pinned images and waits for health.
5. Operator bootstraps the first owner and opens the portal.
6. Upgrade replaces manifests/images but retains data; rollback selects the previous pinned version.

### Upgrade from free projects

1. Organization creates projects within its free allowance.
2. The next creation attempt receives a stable entitlement error and upgrade URL.
3. Owner starts Stripe Checkout for a configured Price.
4. Verified webhook records customer/subscription state and recalculates entitlements.
5. Project creation succeeds. Customer Portal handles later plan/payment-method changes.

### Sell from an FFDB project

1. Project owner configures a project-scoped Stripe account and webhook secret.
2. Application creates an order/cart row and requests Checkout using server-owned Price IDs.
3. Browser redirects to Stripe-hosted Checkout.
4. Verified, replay-safe webhook transitions the order or membership in one durable operation.
5. Application reads the authorized order/subscription state through normal FFDB RLS.

## Naming conventions

| Concept | Canonical label | Notes |
| --- | --- | --- |
| Complete server distribution | Release bundle | Pinned Compose, config, lifecycle command, checksums |
| Default setup | Install FFDB | Never “clone the repo” |
| Contributor workflow | Source installation | Explicitly non-primary |
| FFDB operator charges | Platform billing | Organization/project/seat/usage entitlements |
| A customer app taking money | Project payments | Project-scoped commerce/provider state |
| Recurring provider object | Subscription | Never deprecated “plan” object |
| Billable provider catalog object | Price | Provider identifier mapped to an FFDB entitlement |
| Payment UI | Checkout | Stripe-hosted or embedded Checkout Session |
| Self-service changes | Billing portal | Short-lived on-demand Stripe Customer Portal session |

## Component reuse map

| Component | Used on | Variants |
| --- | --- | --- |
| Install command block | Landing and installation docs | Compact landing vs annotated docs |
| Required-values table | Install, configuration, billing, payments | Secret values are never rendered after write |
| Lifecycle sequence | Sync, platform billing, project payments | Ordered steps with verification points |
| Status callout | Packages, release, billing | Available, release-ready/not published, optional/disabled |
| Portal management panel | Billing and Payments | Organization-scoped vs project-scoped data source |

## Content growth plan

- Add package managers beneath Installation only after automated release publication and verification exists.
- Add payment providers behind the same provider-neutral domain; keep provider-specific setup in child sections.
- Add pricing strategies under Platform billing without changing entitlement API names.
- Keep application commerce recipes separate from core API reference so shops, memberships, donations, and marketplaces can grow independently.

## URL strategy

- Static product concepts use `/docs/<topic>`.
- Scoped families use `/docs/install/*`, `/docs/billing/*`, `/docs/payments/*`, and `/docs/reference/*`.
- No secrets, provider identifiers, organization IDs, or project IDs appear in public-doc URL parameters.
