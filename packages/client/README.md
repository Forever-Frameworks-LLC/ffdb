# `@ffdb/client`

Universal TypeScript client for the current FFDB Rust API. It runs in modern
browsers, Node.js, React Native, and Expo when supplied with the runtime's
`fetch` implementation and an appropriate session store.

FFDB server and SDK releases are version-matched. Pin the package to the exact
server version:

```bash
pnpm add --save-exact @ffdb/client@0.3.2
```

The matching GitHub Release also includes a checksum-listed
`ffdb-client-0.3.2.tgz` for verified offline installation.

```ts
import { BrowserSessionStore, FFDBClient, generateId } from "@ffdb/client";

const client = new FFDBClient({
  baseUrl: "https://api.example.com",
  projectId: "019...",
  sessionStore: new BrowserSessionStore(sessionStorage, "my-app.ffdb"),
});

await client.auth.signIn("user@example.com", "correct horse battery staple");
const result = await client.query({
  sql: "SELECT id, title FROM todos WHERE owner_id = auth.uid()",
});

const id = generateId("todo_");
```

Developer API keys are server/operator credentials. Do not embed them in browser
bundles. End-user requests use the short-lived project session managed by
`client.auth`.

Platform billing uses the authenticated organization methods
`organizationBilling()`, `createBillingCheckout()`, and
`createBillingPortal()`. Application commerce is a separate project-scoped
surface under `client.commerce`. It supports encrypted BYO Stripe credentials
or Stripe Connect direct charges, products, immutable prices, one-time and
recurring Checkout, orders, payments, refunds, subscription cancellation,
customer self-service portal sessions, entitlements, and paid-order fulfillment. Provider credentials are write-only
and never reuse FFDB's own platform-billing customer.

`client.commerce.disconnectAccount()` removes an unused provider configuration
with an idempotent DELETE. The server refuses disconnection while catalog,
customer, order, or subscription records remain provider-bound, preserving
refund, subscription, and reconciliation capability.

```ts
const product = await client.commerce.createProduct({
  name: "Team membership",
  description: "Monthly access for one team",
  tax_code: null,
});
const price = await client.commerce.createPrice({
  product_id: product.id,
  lookup_key: "team_monthly",
  currency: "USD",
  unit_amount_minor: 1500,
  billing: { type: "recurring", interval: "month", interval_count: 1 },
  entitlements: { seats: { type: "quantity", value: 10 } },
});

const portal = await client.commerce.customerPortal({
  subject: { kind: "team", id: teamId },
  return_url: "https://app.example.com/settings/billing",
});
```

## Instance operator lifecycle

Instance administration uses the platform developer session, not a project API
key. Run it only in a trusted operator process; bootstrap tokens, passwords, and
Stripe secrets must never be bundled into browser or native applications.

```ts
import { FFDBClient, MemoryDeveloperSessionStore } from "@ffdb/client";

const operatorSessions = new MemoryDeveloperSessionStore("ffdb-operator");
const operator = new FFDBClient({
  baseUrl: "https://data.example.com",
  developerSessionStore: operatorSessions,
});

const setup = await operator.instanceSetupStatus(); // public, non-secret state
if (setup.bootstrap_available) {
  await operator.developerBootstrap(bootstrapToken, ownerEmail, ownerPassword);
}

await operator.configureInstance({
  deployment_mode: "team",
  organization_creation_policy: "invitation_only",
});
const status = await operator.instanceStatus();
```

For an operator-owned Stripe account, `configureInstance()` accepts
`platform_byo` with write-only `secret_key` and `webhook_secret` values. A
`platform_connect` configuration also requires the operator's write-only Stripe
Connect platform `secret_key` and Connect webhook `webhook_secret`, in addition
to country/email and absolute portal return/refresh URLs. Then call
`createInstanceConnectOnboarding()` and `refreshInstanceBilling()` as required.
Responses expose provider status and capabilities but never return either
supplied credential.

Global administration is available through:

- `updateOrganizationCreationPolicy()`;
- `instanceAdministrators()`, `grantInstanceAdministrator()`, and
  `revokeInstanceAdministrator()`;
- paginated `instanceOrganizations()` and `instanceUsers()`, plus audited
  `setInstanceOrganizationDisabled()` and `setInstanceUserDisabled()` status
  changes;
- `billingExemptions()`, `grantBillingExemption()`, and
  `revokeBillingExemption()`;
- `instancePlans()`, `putInstancePlan()`, and `retireInstancePlan()`.

The immutable owner and deployment/payment configuration remain owner-only;
delegated instance administrators operate the audited global resources
authorized by the API, including plan mutations. Provider readiness,
owner/self-disable safeguards, and protected/in-use plan rules are enforced by
the server.

`InstancePlanCatalogEntry.provider_catalog_bound` identifies a plan whose
provider-priced terms are already verified in Stripe. That flag is read-only
and omitted from `PutInstancePlanCatalogEntryRequest`; provider-bound billing
unit, price/currency, and storage/read/write/MAU terms cannot be edited in place.

## Related packages

- `@ffdb/react`: providers and hooks
- `@ffdb/react-native`: native session and SQLite adapters
- `@ffdb/sync-client`: logical offline synchronization
- `@ffdb/cli`: migrations, administration, scaffolding, and schema type generation

See the repository API reference for the complete method and response types.
