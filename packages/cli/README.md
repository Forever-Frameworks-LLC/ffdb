# `@ffdb/cli`

Command-line workflows for the current self-hosted FFDB Rust API.

FFDB server and CLI releases are version-matched. Pin the CLI to the server
version:

```bash
npm install --global @ffdb/cli@0.3.0
ffdb --help
```

The matching GitHub Release also includes a checksum-listed
`@ffdb/cli-0.3.0.tgz` for verified offline installation.

```bash
FFDB_PASSWORD="$FFDB_PASSWORD" ffdb login developer@example.com
ffdb project link <project-id>
ffdb generate --out src/ffdb.types.ts
```

The CLI stores its platform session in a mode-0600 credential file. A project
developer API key may instead be supplied with `--key` or
`FFDB_DEVELOPER_KEY`. Never place that key in browser-facing environment
variables.

## Instance owner and deployment lifecycle

The public setup check does not need a login and returns only bootstrap state
and non-secret paid-mode availability:

```bash
ffdb --url http://127.0.0.1:5173 instance setup-status
```

On a new installation, load the one-time token and owner password without
placing either value in command history, then bootstrap the immutable first
owner. The command stores only the returned platform session in the mode-0600
CLI credential file; it does not print the session or retain the bootstrap
token/password.

```bash
read -r FFDB_BOOTSTRAP_TOKEN < /root/ffdb-bootstrap-token
read -r -s FFDB_PASSWORD
export FFDB_BOOTSTRAP_TOKEN FFDB_PASSWORD
ffdb --url http://127.0.0.1:5173 instance bootstrap owner@example.com
unset FFDB_BOOTSTRAP_TOKEN FFDB_PASSWORD

ffdb instance status
```

Configure or reconfigure the installation with one of the four modes. The
organization policy is always explicit so a reconfiguration cannot silently
reset it:

```bash
ffdb instance setup private owner_only
ffdb instance configure team invitation_only

# BYO credentials come only from the process environment/secret manager.
read -r -s FFDB_INSTANCE_STRIPE_SECRET_KEY
read -r -s FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET
export FFDB_INSTANCE_STRIPE_SECRET_KEY FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET
ffdb instance configure byo authenticated
unset FFDB_INSTANCE_STRIPE_SECRET_KEY FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET

# Connect platform credentials also come only from the environment/secret manager.
read -r -s FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY
read -r -s FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET
export FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY
export FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET
ffdb instance configure connect owner_only US owner@example.com \
  'https://data.example.com/app/?instance-connect=return' \
  'https://data.example.com/app/?instance-connect=refresh'
unset FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY
unset FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET
```

`private` and `team` do not configure tenant billing. `byo` reads
`FFDB_INSTANCE_STRIPE_SECRET_KEY` and
`FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET`; those values are sent directly to the
API, never written to the credential file, and never included in CLI output.
`connect` similarly reads `FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY` and
`FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET`; it sends those write-only values
directly to the API and returns a short-lived Stripe onboarding URL. After
onboarding, refresh the provider state; once Stripe reports charges and payouts
ready, FFDB provisions or repairs the connected account's plan catalog
automatically.

```bash
ffdb instance connect onboarding \
  'https://data.example.com/app/?instance-connect=return' \
  'https://data.example.com/app/?instance-connect=refresh'
ffdb instance connect refresh
```

A refresh failure is a failed status check, not proof that billing was
activated. Inspect `ffdb instance status`, complete the reported Stripe
requirements, and retry.

An instance with any non-canceled organization billing account is deliberately
locked to its current platform billing mode and Stripe account. Secret rotation
is allowed only when the replacement BYO key belongs to that same account.
Cancel and reconcile every organization subscription before switching between
BYO, Connect, private, or team modes; the API returns
`instance.billing_in_use` when this invariant would be broken.

## Global instance administration

The owner and delegated instance administrators can inspect global inventory
and manage the allowed resources. Only the owner can change the deployment mode
or payment credentials.

```bash
ffdb instance policy set invitation_only
ffdb instance admins list
ffdb instance admins grant <user-id>
ffdb instance admins revoke <user-id>          # prompts
ffdb instance admins revoke <user-id> --yes    # automation

ffdb instance organizations 50 0
ffdb instance users 50 0
ffdb instance org-disable <organization-id>    # prompts
ffdb instance org-enable <organization-id>     # prompts
ffdb instance user-disable <user-id>            # prompts
ffdb instance user-enable <user-id>             # prompts
```

Billing-exemption reasons and plan definitions use JSON files so audited text
and numeric limits do not depend on shell quoting. `exemption.json` contains:

```json
{
  "reason": "Operator-owned organization"
}
```

```bash
ffdb instance exemptions list
ffdb instance exemptions grant <organization-id> exemption.json
ffdb instance exemptions revoke <organization-id> # prompts
```

`pro-plan.json` contains the complete mutable plan definition:

```json
{
  "display_name": "Pro",
  "billing_unit": "organization",
  "base_price_cents": 4900,
  "currency": "usd",
  "project_limit": null,
  "storage_bytes": 100000000000,
  "monthly_reads": 100000000,
  "monthly_writes": 10000000,
  "monthly_active_users": 100000,
  "overage_enabled": true,
  "reads_at_limit": "overage",
  "writes_at_limit": "overage",
  "signups_at_limit": "overage",
  "requires_payment_method_for_overage": true,
  "active": true
}
```

```bash
ffdb instance plans list
ffdb instance plans put pro pro-plan.json
ffdb instance plans retire pro # prompts and remains server-policy protected
```

Plans returned with `provider_catalog_bound: true` are tied to a verified Stripe
catalog. Their billing unit, base price, currency, and provider-priced
storage/read/write/MAU allowances cannot be changed in place; provision and
verify a replacement provider price instead. Other policy fields remain subject
to server validation.

Administrator revocation, user/organization enablement changes, exemption
removal, and plan retirement require an interactive confirmation unless
`--yes` is supplied. These global status changes are audited by the API. The
API still rejects owner disable/revocation, unsafe self-disable, protected
Free-plan retirement, in-use plan retirement, and other invalid transitions.

## Replacement workflows

- `ffdb init <directory> [browser|react|node]` writes a small integration
  template and `.env.example` without installing dependencies or embedding a
  secret.
- `ffdb generate [--out path]` (also `ffdb types generate`) reads the live
  project schema and produces application table interfaces.
- `ffdb migration create|status|apply|rollback` manages explicit reversible
  migrations.
- `ffdb sql`, `schema`, `policies`, `auth`, `storage`, `email`, `backup`, and
  organization/project commands map directly to supported Rust API routes.
- `ffdb billing status|usage|invoices|checkout|portal` manages organization-scoped FFDB
  subscription redirects.
- `ffdb commerce` configures the linked project's encrypted BYO Stripe keys or
  Connect onboarding and manages products, prices, orders, payments, refunds,
  subscriptions, entitlements, and paid fulfillment. `configure-byo` reads
  `FFDB_COMMERCE_STRIPE_SECRET_KEY` and
  `FFDB_COMMERCE_STRIPE_WEBHOOK_SECRET` so secrets do not enter shell history.
- `ffdb instance` covers first-owner discovery/bootstrap, deployment and
  operator-billing configuration, Connect recovery, global administrators,
  users/organizations, billing exemptions, and the plan catalog. Instance
  billing is separate from linked-project commerce.

Type generation is conservative. SQLite declared types are mapped
to their FFDB wire representation, nullable columns include `null`, BLOBs use
`BlobValue`, and expressions whose shape cannot be recovered from `CREATE TABLE`
SQL become `unknown`. Generated types are compile-time aids; the server remains
authoritative for SQL parsing, RLS, resource limits, and result metadata.

The previous CLI's Better Auth/Kysely templates are not copied because those
assumptions conflict with the current authentication and query boundaries.
