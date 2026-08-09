# Billing and project payments

FFDB keeps two payment domains deliberately separate:

- **Platform billing** belongs to an organization and pays for FFDB itself.
  Organization members can read billing state; only owners and admins can create
  Stripe Checkout or Customer Portal sessions.
- **Project commerce** belongs to one customer project. Each project chooses
  encrypted bring-your-own Stripe credentials or optional Stripe Connect direct
  charges. Products, immutable prices, Checkout, orders, subscriptions,
  entitlements, payments, refunds, fulfillment, and verified webhooks share one
  provider-neutral API above either mode.

## Platform catalog and enforcement

Migration `0006_platform_billing` seeds configurable Free, Pay as you go, and
Pro catalog rows. Free includes two projects plus 1 GB storage, 1 million monthly
reads, 50,000 monthly writes, and 5,000 monthly active users without overage.
PAYG starts with the same allowance and enables configured overage. Pro defaults
to $7/month with 10 GB, 15 million reads, 750,000 writes, and 50,000 MAU plus
overage. The seed policy records that Free reads continue while writes and new
signups pause at their limits, and that overage always requires a payment method.

The two-project Free allowance is enforced transactionally under an organization
row lock. Only `active` and `trialing` paid subscriptions lift it. Successful
data operations produce idempotent worker receipts; FFDB records read, write,
logical-storage, and monthly-active-user usage in one private SQLite database per
organization below `FFDB_METRICS_ROOT`. Reservations enforce write, signup, and
storage admission without double counting retries. Durable reporting outbox and
reconciliation state remain with those usage records, so the metrics root is a
billing ledger and must be included in backup, restore, access-control, and disk
capacity procedures.

## HTTP surface

- `GET /v1/organizations/{organization_id}/billing`
- `GET /v1/organizations/{organization_id}/billing/usage`
- `GET /v1/organizations/{organization_id}/billing/invoices`
- `POST /v1/organizations/{organization_id}/billing/checkout`
- `POST /v1/organizations/{organization_id}/billing/portal`
- `POST /v1/billing/webhooks/stripe`
- `GET /v1/projects/{project_id}/commerce/account`
- `DELETE /v1/projects/{project_id}/commerce/account`
- `POST /v1/projects/{project_id}/commerce/account/byo`
- `POST /v1/projects/{project_id}/commerce/account/connect/onboarding`
- `POST /v1/projects/{project_id}/commerce/account/refresh`
- `GET|POST /v1/projects/{project_id}/commerce/products`
- `GET|POST /v1/projects/{project_id}/commerce/prices`
- `POST /v1/projects/{project_id}/commerce/checkouts/one-time`
- `POST /v1/projects/{project_id}/commerce/checkouts/recurring`
- `POST /v1/projects/{project_id}/commerce/customer-portal`
- `GET /v1/projects/{project_id}/commerce/orders`
- `GET /v1/projects/{project_id}/commerce/payments`
- `POST /v1/projects/{project_id}/commerce/refunds`
- `GET /v1/projects/{project_id}/commerce/subscriptions`
- `POST /v1/projects/{project_id}/commerce/subscriptions/{subscription_id}/cancel`
- `GET /v1/projects/{project_id}/commerce/entitlements`
- `PATCH /v1/projects/{project_id}/commerce/orders/{order_id}/fulfillment`
- `POST /v1/projects/{project_id}/commerce/webhooks/stripe`
- `POST /v1/commerce/webhooks/stripe-connect`

Checkout and portal mutations require `Idempotency-Key`. The Stripe adapter uses
hosted Checkout Sessions in subscription mode with Price IDs, Stripe Billing,
and the Customer Portal. It sends Stripe API version `2026-02-25.clover`; it does
not use Charges, Sources, Plans, or legacy Connect account-type labels.

Webhooks are verified against the exact raw body using `Stripe-Signature` HMAC,
a five-minute timestamp tolerance, and the configured test/live mode. Provider
event IDs and payload hashes are stored transactionally. Exact retries are
acknowledged, ID reuse with different bytes is rejected, out-of-order events do
not regress newer state, and platform metadata cannot update a project-commerce
account. Customer and subscription identifiers are permanently tenant-bound.

## Instance billing setup

The primary setup path is **Global admin**, not a hand-built Stripe catalog.
Choose one of these instance modes:

- **Private** and **Team** keep the organization usage ledgers and quota
  summaries active without charging organizations.
- **Stripe BYO** accepts the instance owner's Stripe secret key and the signing
  secret for `POST /v1/billing/webhooks/stripe` over the authenticated setup
  request. FFDB encrypts both at rest, checks the target account, and creates or
  recovers the FFDB Product, Pro base Price, four Billing Meters, and separate
  PAYG/Pro metered Prices with stable idempotency keys.
- **Stripe Connect** creates an Accounts v2 connected merchant from Global
  admin, completes hosted onboarding, and provisions the same catalog in that
  connected account. The FFDB host needs this dedicated platform pair:

```dotenv
FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY=sk_live_...
FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET=whsec_...
```

Create the instance Connect webhook endpoint in Stripe with URL
`https://YOUR_FFDB_ORIGIN/v1/billing/webhooks/stripe`. Connected-mode events
must carry the exact configured `event.account`; BYO events must not carry a
connected account. Both modes verify the exact raw request body, signature,
live/test mode, event ID, and payload hash before changing an entitlement.

Provisioned Price fields are provider-bound. Global admins can read the
`provider_catalog_bound` flag and change non-provider policy, but attempts to
make the local amount, currency, included usage, or billing unit diverge from
the active Stripe catalog fail with `instance.plan_provider_bound`. Switch out
of the paid mode before replacing that catalog; FFDB never silently mutates an
active provider contract.

Storage uses decimal units throughout: Free/PAYG include 1,000,000,000 bytes and
Pro includes 10,000,000,000 bytes. FFDB reports provider usage in decimal
kilobyte-hours (1 kB = 1,000 bytes). The storage overage rate is
`0.000000027397` cents per kB-hour, which is approximately $0.20 per decimal
GB-month at 730 hours. The default provider event is
`ffdb_storage_kilobyte_hours`; the internal durable metric name remains
`storage_byte_hours` because the ledger records byte-time before provider-unit
conversion.

Existing installations may import an already-created Stripe catalog by setting
the compatibility values below. FFDB validates the identifiers against the BYO
account selected in Global admin before it uses them; if no imported catalog is
present, automatic provisioning is used. Configure the whole catalog together:

```dotenv
FFDB_STRIPE_SECRET_KEY=sk_live_...
FFDB_STRIPE_WEBHOOK_SECRET=whsec_...
FFDB_STRIPE_PRO_BASE_PRICE_ID=price_...
FFDB_STRIPE_READS_EVENT_NAME=ffdb_reads
FFDB_STRIPE_READS_METER_ID=mtr_...
FFDB_STRIPE_PAYG_READS_PRICE_ID=price_...
FFDB_STRIPE_PRO_READS_PRICE_ID=price_...
FFDB_STRIPE_WRITES_EVENT_NAME=ffdb_writes
FFDB_STRIPE_WRITES_METER_ID=mtr_...
FFDB_STRIPE_PAYG_WRITES_PRICE_ID=price_...
FFDB_STRIPE_PRO_WRITES_PRICE_ID=price_...
FFDB_STRIPE_STORAGE_EVENT_NAME=ffdb_storage_kilobyte_hours
FFDB_STRIPE_STORAGE_METER_ID=mtr_...
FFDB_STRIPE_PAYG_STORAGE_PRICE_ID=price_...
FFDB_STRIPE_PRO_STORAGE_PRICE_ID=price_...
FFDB_STRIPE_MAU_EVENT_NAME=ffdb_monthly_active_users
FFDB_STRIPE_MAU_METER_ID=mtr_...
FFDB_STRIPE_PAYG_MAU_PRICE_ID=price_...
FFDB_STRIPE_PRO_MAU_PRICE_ID=price_...
FFDB_STRIPE_PRO_BILLING_UNIT=organization
FFDB_BILLING_SUCCESS_URL=https://portal.example.com/app/billing/success
FFDB_BILLING_CANCEL_URL=https://portal.example.com/app/billing/cancel
FFDB_BILLING_PORTAL_RETURN_URL=https://portal.example.com/app/billing
```

Meter aggregation must be `sum`. PAYG and Pro need distinct Price IDs because
their included tiers differ; the Pro base Price is the non-metered subscription
item. `FFDB_STRIPE_PRO_BILLING_UNIT` may be `organization` or `seat`; FFDB derives
seat quantity from durable membership rather than accepting caller input.
Secrets must never be placed in `VITE_*`, `NEXT_PUBLIC_*`, or `EXPO_PUBLIC_*`
variables.

Project commerce uses an explicit account mode. Connect always uses direct
charges, so the connected project owner is merchant of record; BYO requests use
that project's decrypted key only for the duration of the provider call. The
two modes are mutually exclusive, optional, and replaceable through an audited
account reconfiguration flow. Credential rotation or a mode change targeting
the same Stripe account preserves the catalog. Rebinding to a different Stripe
account is rejected after any provider-bound commerce state exists, because
Stripe Product, Price, Customer, Payment, and Subscription IDs are account-
scoped. No secret is returned after configuration.

BYO mode is available on every deployment. Optional project Connect onboarding
uses a second platform credential pair that is never used for the FFDB instance's
own organization subscriptions:

```dotenv
FFDB_COMMERCE_STRIPE_CONNECT_SECRET_KEY=sk_live_...
FFDB_COMMERCE_STRIPE_CONNECT_WEBHOOK_SECRET=whsec_...
```

Configure that Stripe Connect webhook endpoint as
`https://YOUR_FFDB_ORIGIN/v1/commerce/webhooks/stripe-connect`. A BYO project
instead uses its account-specific endpoint at
`/v1/projects/{project_id}/commerce/webhooks/stripe`. Never reuse either project
endpoint secret as `FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET`; Stripe signs
each endpoint independently and FFDB enforces the account boundary.

Use `client.commerce.configureByo()` from a trusted operator environment, or
`client.commerce.connectOnboarding()` to create an Accounts v2 merchant with
explicit Stripe fee/loss responsibility and full Dashboard access. All project
charges are direct charges. Catalog reads are public; management mutations need
a platform owner/admin session or a developer key with `commerce_manage`.
Checkout and refund mutations require durable `Idempotency-Key` values.

The BYO project webhook and the global Connect webhook both verify the exact raw
body, livemode, payload hash, and five-minute signature timestamp. The Connect
endpoint verifies its dedicated endpoint secret before parsing `event.account`,
then resolves the unique connected project and rechecks the account binding.
The BYO endpoint rejects Connect events. Their durable inbox rejects
event-ID hash conflicts and applies payment, invoice, subscription, and refund
events in provider-created order. Checkout redirects never grant access;
verified subscription state is the authority for entitlements, and fulfillment
cannot advance to processing or fulfilled until captured funds cover the order.

An unused account configuration can be removed with
`client.commerce.disconnectAccount()` or `ffdb commerce disconnect --yes`.
This audited operation deletes only FFDB's local account binding and encrypted
project secrets; it never closes a Stripe account. Once any product, customer,
order, or subscription has bound provider state to the project, FFDB returns
`commerce.account_in_use` and preserves the full configuration.
