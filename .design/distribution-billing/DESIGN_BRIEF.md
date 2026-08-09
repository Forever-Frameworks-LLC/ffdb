# FFDB distribution and billing brief

## Product decision

FFDB is installed and operated as a versioned product release. A source checkout is a contributor workflow, not the default customer journey.

The public product also treats billing as two distinct first-class domains:

1. **FFDB platform billing** controls what an organization owes the FFDB operator for projects, seats, usage, or a managed plan.
2. **Project payments** lets an application built on FFDB accept one-time purchases and subscriptions for its own customers without mixing its provider account, customers, webhooks, or money movement with FFDB platform billing.

## Primary audiences

- An operator who wants to install one supported FFDB release on a Linux host without cloning or building the repository.
- An application developer who wants published SDK/CLI artifacts and end-to-end examples rather than workspace-only commands.
- A product owner who needs free allowances, metered or subscription pricing, organization membership entitlements, and application commerce.
- A contributor who intentionally chooses source installation, local providers, and repository development commands.

## Required outcomes

- A release bundle installs a pinned set of FFDB images, configuration templates, and lifecycle commands without a repository checkout.
- The default landing and documentation path starts with the release installer; Docker bundle download is the transparent/manual alternative.
- npm distributes SDKs and the CLI; Cargo and Homebrew distribute appropriate components but do not pretend to provision PostgreSQL, object storage, TLS, and durable volumes by themselves.
- Every documentation page answers what, why, when, prerequisites, required values, a working example, verification, common failures, and next steps.
- Platform billing begins with a configurable free-project allowance (default two), paid entitlements, Stripe Checkout/Billing/Customer Portal, and replay-safe webhook state.
- Project payments use project-scoped provider configuration and APIs. Platform billing secrets and customer-project payment secrets never share a tenant namespace.
- Payment fulfillment follows verified provider events and durable idempotency; browser redirects are not proof of payment.

## Constraints

- Preserve the accepted landing, docs, and portal visual systems; this is a product/content expansion inside an existing design system, so no new visual concept is required.
- Do not claim that images, npm packages, Homebrew formulae, install URLs, or paid plans are publicly available until a release pipeline has published them.
- Use current Stripe Checkout Sessions, Billing Prices, Customer Portal, Setup Intents when needed, raw-body webhook verification, and Accounts v2 for new Connect work.
- Do not use Charges, Sources, deprecated Plans, or legacy connected-account type labels.
- Self-hosted operators can disable billing. Enabling project-count enforcement is an explicit deployment choice.

## Acceptance

- A clean Linux host can install, configure, start, inspect, upgrade, roll back, and uninstall FFDB from a release bundle while preserving data by default.
- Release assets are checksum-verifiable and container tags are version-pinned.
- SDK/CLI publish metadata and release automation produce inspectable package artifacts.
- Documentation depth is enforced by tests so a page cannot regress to a title and one paragraph.
- Billing models, migrations, API/client/CLI contracts, webhook verification, isolation, and idempotency have automated tests.
- Landing and docs examples exactly match shipped commands and implemented status.
