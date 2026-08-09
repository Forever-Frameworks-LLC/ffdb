# TypeScript Client SDK

`@ffdb/client` is the sole HTTP implementation. It supports browser/Node/RN-
compatible fetch, project auth and refresh deduplication, parameterized SQL,
transactions, deterministic result types, schema/policies/migrations, storage,
sync, email templates, cancellation, bounded retries, and idempotency keys.

The default session store is in memory. Browser storage is opt-in because XSS can
read it; hardened apps should prefer secure cookie/BFF patterns. React Native must
provide a secure credential adapter and never assumes DOM, Window, or localStorage.

`@ffdb/react` provides auth/query/sync state hooks and providers without serial
fetch waterfalls. `@ffdb/react-native` supplies runtime-neutral types/adapters for
native secure storage and SQLite. `@ffdb/sync-client` coordinates snapshot,
pending mutations, optimistic local rows, push/pull, tombstones, rejection
inspection, and resnapshot through a transactional replica adapter. Its
`getRow()` and `listRows()` methods provide the same safe typed read surface in
browser, Node, React Native, and memory runtimes. The server remains the SQL and
authorization authority.

Use an `AbortSignal` for interactive work. Automatic retry applies only to safe
reads or mutations carrying a stable idempotency key and uses bounded jitter plus
`Retry-After`. A refresh is single-flight; refresh reuse/rejection clears the local
session.

## Trusted instance administration

The same client exposes a platform-session surface for trusted operator tools.
`instanceSetupStatus()` is deliberately public and returns only first-owner
state plus non-secret BYO/Connect availability. `developerBootstrap()` creates
the immutable first owner and persists its returned platform session through the
configured `DeveloperSessionStore`; do not use it in an application bundle.

Authenticated owners can read `instanceStatus()` and call
`configureInstance()` for private, team, platform-BYO, or platform-Connect
deployment. BYO Stripe fields are write-only. Connect onboarding uses explicit
absolute return/refresh URLs and is reconciled with
`refreshInstanceBilling()`; a failed refresh must not be interpreted as billing
activation.

Instance owners and delegated administrators can use the administrator,
paginated organization/user inventory, billing-exemption, and plan-catalog
methods documented by the exported TypeScript types. Global user and
organization enablement changes use the audited
`setInstanceUserDisabled()`/`setInstanceOrganizationDisabled()` methods. The
owner alone can change deployment or provider credentials; delegated admins can
mutate plans. The API remains authoritative for role, immutable-owner,
self-disable, catalog, protected-plan, and provider-readiness constraints.

Plan reads expose `provider_catalog_bound`. It is omitted from plan write input;
when true, Stripe-priced billing unit, base price/currency, and
storage/read/write/MAU terms require a verified replacement provider price
rather than an in-place mutation.
