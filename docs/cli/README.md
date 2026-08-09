# CLI

`@ffdb/cli` exposes project and development workflows through the public SDK. The
command surface includes login, organizations/projects, API-key lifecycle, project
linking, migration create/apply/status/rollback, schema/policy inspection,
developer SQL, seeds, local development, auth/email/storage configuration, logs,
health, and backup/restore.

Project-scoped `ffdb backup` commands operate one encrypted project backup. They
are distinct from the root-only host disaster-recovery commands installed with a
release: `ffdb-host backup create /absolute/output.tar.gz` for packaged
single-host Docker, and `ffdb-backup create /absolute/output.tar.gz` for native
systemd. See the [host backup runbook](../operations/backup-restore.md).

Migration files contain a stable id/name and explicit `up` and `down` SQL. The CLI
calculates and preserves the server checksum and refuses an edited applied id.
Credentials are written atomically to the configured credential file with
owner-only permissions. They are never written into migration/project files or
debug logs. Avoid passing a key through a shell-history-visible flag; prefer the
credential file or `FFDB_DEVELOPER_KEY`. JSON output is available for automation,
while a newly created secret is returned only by its creation command.

Point the first login at the public FFDB origin, for example
`ffdb --url http://127.0.0.1:5173 login developer@example.com` for the packaged
single-host profile. That port is the compiled nginx gateway, not Vite; nginx
proxies the request to Axum on the private Compose network. Port `8080` is a
direct Axum diagnostic only in the contributor Compose model and is not a
packaged Docker ingress. A successful login persists the selected base URL.

Project creation and the migration/backup operations documented as replay-safe
use idempotency keys. Other mutations are not retried automatically. Destructive
rollback/restore/key revocation commands require explicit confirmation unless a
non-interactive confirmation flag is supplied.

## Instance lifecycle commands

`ffdb instance` administers the installation itself. It is not project-scoped,
and its Stripe account is separate from `ffdb commerce` for a linked
application.

Start by checking the unauthenticated setup state and non-secret host
capabilities:

```bash
ffdb --url https://data.example.com instance setup-status
```

For a fresh installation, provide the bootstrap token and password only through
the environment. `instance bootstrap` writes the returned owner session to the
mode-0600 credential file and prints only non-secret owner/session metadata.

```bash
read -r FFDB_BOOTSTRAP_TOKEN < /root/ffdb-bootstrap-token
read -r -s FFDB_PASSWORD
export FFDB_BOOTSTRAP_TOKEN FFDB_PASSWORD
ffdb --url https://data.example.com instance bootstrap owner@example.com
unset FFDB_BOOTSTRAP_TOKEN FFDB_PASSWORD
```

The authenticated lifecycle surface is:

```text
instance status
instance setup|configure private <policy>
instance setup|configure team <policy>
instance setup|configure byo <policy>
instance setup|configure connect <policy> <country> <email> <return-url> <refresh-url>
instance policy set <policy>
instance connect onboarding <return-url> <refresh-url>
instance connect refresh

instance admins list|grant|revoke
instance organizations [limit] [offset]
instance users [limit] [offset]
instance org-disable|org-enable <organization-id> [--yes]
instance user-disable|user-enable <user-id> [--yes]
instance exemptions list|grant|revoke
instance plans list|put|retire
```

Project commerce account lifecycle commands are:

```text
commerce status
commerce configure-byo
commerce connect <country> <email> <return-url> <refresh-url>
commerce refresh
commerce disconnect --yes
```

`commerce disconnect` is an audited local disconnect. It deletes an unused
FFDB provider binding and encrypted BYO secrets, but never closes the external
Stripe account. It fails with `commerce.account_in_use` once provider-bound
catalog, customer, order, or subscription state exists.

`<policy>` is `owner_only`, `authenticated`, or `invitation_only`. BYO mode
reads `FFDB_INSTANCE_STRIPE_SECRET_KEY` and
`FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET`; the CLI neither prints nor persists
these values. Connect return and refresh values must be absolute HTTP(S) URLs.
After Stripe onboarding, `instance connect refresh` rechecks capabilities and,
once ready, provisions or repairs the account's plan catalog. A failed refresh
does not activate billing.

Exemption grants accept a JSON file containing `{ "reason": "..." }`. Plan
puts accept the full mutable plan object described by
`PutInstancePlanCatalogEntryRequest` in `@ffdb/client`, including display name,
billing unit, price/currency, project/storage/read/write/MAU limits, overage and
at-limit behaviors, payment-method policy, and active state:

```bash
ffdb instance exemptions grant <organization-id> exemption.json
ffdb instance plans put <free|pay_as_you_go|pro> plan.json
```

`provider_catalog_bound` is returned by plan reads and is not a writable input.
When true, the provider-priced billing unit, base price, currency, and
storage/read/write/MAU fields are immutable; a replacement Stripe price must be
provisioned and verified rather than mutating customer-visible terms in place.

Administrator revocation, global user/organization enablement changes,
exemption removal, and plan retirement prompt for confirmation; use `--yes`
only for reviewed automation. Status changes are audited by the API. Server
rules remain authoritative for immutable ownership, unsafe self-disable,
protected/in-use plans, role boundaries, provider readiness, and catalog
verification.
