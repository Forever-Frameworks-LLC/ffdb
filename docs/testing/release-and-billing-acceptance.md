# Release, billing, and payments acceptance plan

Run this plan against a disposable release-candidate host and Stripe test mode. Never use production provider secrets or a production billing account.

## Evidence to retain

- release version, architecture, bundle URL, and SHA-256 manifest;
- installed Compose config with secret values redacted;
- container image digests, health output, and lifecycle-command output;
- PostgreSQL row counts and durable-volume identifiers before/after upgrade and rollback;
- Stripe test event IDs, FFDB request IDs, webhook processing status, and resulting entitlement snapshots;
- npm/Cargo/Homebrew artifact names and checksums;
- screenshots for landing install/pricing, docs installation/billing, and portal Billing/Payments routes.

## Release bundle

### REL-01 — Install without source

1. Start from a Linux host with Docker Engine and Compose v2 but without Git, Rust, Node, pnpm, or the FFDB repository.
2. Download the installer to a file, inspect it, and run it as documented.
3. Confirm `/opt/ffdb` contains only release-managed files and `/etc/ffdb` contains owner-only configuration.
4. Confirm `ffdb-host` is on the operator path and reports the installed version.
5. Confirm every FFDB image is pinned to the selected version or digest rather than `latest`.
6. From a non-browser network, curl the GitHub Release
   `latest/download/{install.sh,uninstall.sh,stable.txt}` URLs and every asset at
   `releases/download/vVERSION/ASSET`.
   Reject HTTP 403, interactive bot-challenge redirects/pages, HTML in script or
   checksum responses, and any checksum/signature mismatch.

Pass: the operator can install and inspect the product without a source checkout
or build toolchain, and no interactive challenge blocks the documented
curl-only GitHub Releases installation path.

### REL-02 — Integrity failure

1. Serve a test bundle whose content does not match its checksum manifest.
2. Run the installer with `FFDB_RELEASE_BASE_URL` pointing at that fixture.

Pass: installation stops before replacing any active release, starting containers, or changing configuration.

### REL-03 — Configuration ownership

1. Complete installation without providing production values.
2. Inspect generated files and run `ffdb-host start`.

Pass: secrets are not printed; config is owner-readable only; placeholders fail preflight with actionable names; the service does not start in a partially configured production state.

### REL-04 — Start, status, logs, and stop

1. Run `ffdb-host start` twice.
2. Run `ffdb-host status` and `ffdb-host logs`.
3. Run `ffdb-host stop` twice.

Pass: lifecycle operations are idempotent, status distinguishes running/healthy/not-ready, logs do not expose secrets, and stop retains every named volume.

### REL-05 — Upgrade and rollback

1. Create an organization, two projects, one auth user, rows, an object, a pending
   sync mutation, an encrypted backup, and usage in the organization's metrics
   ledger.
2. Record values and image identities.
3. Upgrade to the next signed fixture release.
4. Verify migrations, health, and all recorded data.
5. Roll back to the previous compatible release.

Pass: release files and images change; configuration, project/backup/sync data,
and the `metrics-data` billing ledger remain; incompatible database downgrades
stop with an explicit recovery instruction rather than silently running old code.

### REL-06 — Uninstall safety

1. Run `ffdb-host uninstall` without a data-destruction option.
2. Reinstall the same release and configuration.

Pass: release-managed files/services are removed, durable data remains, and the restored deployment sees the previous organizations/projects. Any permanent-data removal requires a distinct, explicit, confirmed operation.

### REL-07 — Complete host backup and restore

1. On a disposable packaged `single-host` installation, create sentinel rows in
   a project SQLite database and the organization's usage ledger, upload an
   object, create an encrypted project backup, and record PostgreSQL control-plane
   state.
2. Run `sudo ffdb-host backup create /secure/ffdb-acceptance.tar.gz`. Confirm the
   archive is mode `0600`, no secret value appears in command output, all services
   resume, and a second create to the same path is refused.
3. Change every sentinel. Stop the host. Attempt restore without `--yes`, with a
   checksum-corrupted copy, and with a different installed FFDB version. Confirm
   every attempt fails before data, volumes, or configuration changes.
4. Run `sudo ffdb-host backup restore /secure/ffdb-acceptance.tar.gz --yes`.
   Confirm project/metrics SQLite `quick_check`, PostgreSQL state, MinIO object,
   encrypted backup, sync state, configuration ownership, and gateway readiness.
5. Repeat with a native Linux bundle using `ffdb-backup`; protect external S3 at
   the same recovery point and verify API plus nginx readiness after restore.

Pass: one exact-version archive recovers all state owned by its installation
profile; create always resumes the original mutation services even on injected
failure; validation precedes destructive restore; and no incomplete
external-provider backup is presented as complete.

## Package channels

### PKG-01 — Release tarballs and public npm packages

For all six packages (`@ffdb/client`, `@ffdb/cli`, `@ffdb/sync-client`,
`@ffdb/react`, `@ffdb/react-native`, and `@ffdb/email-components`):

1. Build and `npm pack --dry-run --json` from a clean release checkout.
2. Confirm the matching GitHub Release contains all six checksum-listed tarballs.
3. Install every tarball into a new external fixture project.
4. Verify all six `@ffdb/*@VERSION` packages are published to npm with
   provenance and resolve to the same release version.
5. Import every documented public export in Node ESM and bundle the browser-compatible packages.
6. Confirm source/tests/secrets/workspace paths are not shipped accidentally.

Pass: the packed artifact is self-contained, version-aligned, and matches documentation. Publishing remains blocked unless registry provenance and release version match the tag.

### PKG-02 — CLI package

1. Install the packed CLI globally into an isolated npm prefix.
2. Run `ffdb --help`, `ffdb health`, login, project list, type generation, and one migration workflow.

Pass: no workspace path is required and credential files remain owner-only.

### PKG-03 — Cargo and Homebrew components

1. Install each provided binary/formula artifact on every supported architecture.
2. Confirm documentation labels it as a component install unless it also provisions every required dependency and lifecycle file.

Pass: component package managers never imply that installing one binary also configured PostgreSQL, S3, TLS, volumes, email, or the web gateway.

## Platform billing

### BILL-01 — Billing disabled

1. Start FFDB without Stripe variables.
2. Request an organization billing summary.
3. Attempt Checkout and Customer Portal creation.

Pass: the summary returns Free entitlements and `project_limit=2`; provider mutations return `503 billing.provider_unavailable`; secrets are not required for a self-hosted operator that does not want billing.

### BILL-02 — Transactional free-project allowance

1. Create two active projects in one Free organization.
2. Concurrently attempt two additional project creations.
3. Archive/delete one project only if the documented policy says that releases capacity, then retry.

Pass: no more than two billable active projects exist; denied attempts return the stable entitlement error; another organization is unaffected.

### BILL-03 — Checkout authorization and idempotency

1. As viewer/developer, attempt Checkout creation.
2. As owner/admin, submit without `Idempotency-Key`, then twice with the same key and body, then reuse the key with a different body.

Pass: insufficient roles and missing keys fail; an exact retry returns the same logical result; conflicting reuse fails; only one provider Checkout Session exists.

### BILL-04 — Verified webhook entitlement

1. Send a fixture event without a signature, with a bad signature, outside the timestamp tolerance, and with modified raw bytes.
2. Send a valid signed `checkout.session.completed` or subscription event.
3. Deliver the same event multiple times and deliver an older subscription state after a newer event.

Pass: only valid raw-body signatures are accepted; one event ID is applied once; stale/replayed delivery cannot regress the entitlement; provider IDs are unique within their billing domain.

### BILL-05 — Plan lifecycle

Exercise Free → pay-as-you-go → Pro → past_due → active → cancel-at-period-end → canceled using Stripe test events.

Pass: organization summary, project allowance, included-usage policy, billing
unit, effective timestamps, and accumulated read/write/storage/MAU usage match
the latest verified state through every transition. Free admission follows the
catalog policy; active paid tiers retain usage and enable configured overage.

### BILL-06 — Customer Portal

1. Request a portal session for an organization with no provider customer.
2. Request one after Checkout creates the customer.
3. Verify return URL allowlisting and short lifetime.

Pass: missing customers get a stable actionable error; only owner/admin can create a portal session; URLs are never persisted as long-lived credentials or logged.

### BILL-07 — Tenant separation

1. Use an owner from organization A to request organization B billing, Checkout, and portal state.
2. Attempt to reuse provider IDs, idempotency keys, and webhook metadata across organizations.

Pass: authorization and database constraints prevent cross-organization reads, mutations, and provider-object reassignment.

### BILL-08 — Durable usage accounting and retry safety

1. Record successful read and write queries, a mixed transaction, snapshot, sync
   pull, and sync push containing accepted, duplicate, and rejected mutations.
2. Retry each operation with its original idempotency key/request receipt and
   also send a deliberate conflicting reuse.
3. Repeat activity as the same end user and as a second user, update database and
   object sizes, then cross an hourly bucket and monthly billing boundary.
4. Restart the API between worker completion and client retry, then inspect the
   organization summary and private metrics database with operator-only tooling.

Pass: only successful statement units and accepted nonduplicate sync mutations
are counted; exact retries return the original receipt without replay or double
counting; conflicting reuse fails; MAU deduplicates one subject per billing
period without storing the raw subject; logical storage integrates to byte-hours;
and no user SQL/API exposes the private receipt or organization-metrics tables.

### BILL-09 — Meter reporting, retry, reconciliation, and cutoff

1. In Stripe test mode, generate usage in all four dimensions and wait for the
   next reporting boundary.
2. Interrupt delivery after durable outbox enqueue, after Stripe accepts an
   event, and before local acknowledgement; restart and allow retry.
3. Force provider 429 and 5xx responses, inspect bounded backoff, then recover.
4. Reconcile provider totals for the closing period, inject one mismatch, and
   exercise the warning and hard-cutoff timestamps.

Pass: stable Stripe identifiers make every retry idempotent; acknowledged
quantities are never resent as new usage; all four dimensions reconcile before
period finalization; degraded state is visible; and the documented hard cutoff
pauses billable writes without hiding already recorded reads or corrupting the
ledger.

### BILL-10 — Metrics-ledger backup and coordinated restore

1. Use `ffdb-host backup create` on packaged single-host or `ffdb-backup create`
   on native systemd to capture the complete metrics root and PostgreSQL billing
   state at one recorded recovery point. For external providers, quiesce ingress
   and coordinate the metrics snapshot with the provider backup.
2. Add later usage and provider acknowledgements, restore the pair into an
   isolated environment, and retain the original as evidence.
3. Run SQLite `quick_check` for every organization ledger and reconcile reads,
   writes, storage byte-hours, and MAU with the restored PostgreSQL/provider
   checkpoint before enabling billable writes.
4. Separately restore one project backup without restoring the metrics root.

Pass: the coordinated restore reproduces the selected billing state with correct
ownership and no cross-organization files; later history is bounded and recorded
as expected data loss; and a project-only restore does not roll organization
usage backward.

## Project payments

### PAY-01 — Domain separation

1. Inspect platform billing and project commerce storage/API responses.
2. Configure or simulate the same Stripe account/customer/event identifiers in both domains.

Pass: tables, provider-account references, webhook namespaces, idempotency keys, and secrets remain separate. A platform billing webhook cannot fulfill a project order or membership.

### PAY-02 — Project capability status

1. Query project payment status before configuration and with insufficient project scope.
2. Configure BYO Stripe test keys as a project owner/admin, refresh the account,
   then remove the configuration.
3. On a second project, begin Stripe Connect onboarding, complete requirements,
   refresh the account, and remove the connection.
4. Create a provider-bound Product, then attempt the audited disconnect again;
   verify `commerce.account_in_use`, unchanged account/secrets, and an audit
   failure record. Verify a successful unused disconnect removes both local
   account and BYO secret rows without closing the external Stripe account.

Pass: unconfigured is an explicit state rather than a fake success; end users cannot inspect provider configuration; secret material is write-only and absent from responses/logs; disconnect is atomic, audited, idempotent, and guarded by provider-bound state.

### PAY-03 — One-time shop Checkout

1. Create a server-owned product/Price mapping and a pending order protected by RLS.
2. Attempt to submit a browser-authored amount, currency, destination account, or success entitlement.
3. Create Checkout using only allowed product/Price and quantity values.
4. Complete, expire, and replay the session events.

Pass: the server owns monetary values and provider routing; the redirect is not fulfillment; a verified event transitions the correct order exactly once.

### PAY-04 — Membership subscription

1. Map a Price to a user, organization, or team membership entitlement.
2. Test active, trialing, past_due, canceled, upgrade, downgrade, and seat quantity events.
3. Verify RLS for members and non-members before and after each transition.

Pass: membership follows the latest verified subscription state and reference type; canceled/past-due policy is explicit; seat counts cannot be self-asserted by the client.

### PAY-05 — Marketplace/Connect

1. Create Accounts v2 fixtures with explicit responsibility, dashboard, and capability configuration.
2. Verify direct charges consistently use the connected account request scope;
   create its Product, Price, Customer, Checkout, Payment, Refund, Subscription,
   and webhook objects on that account.
3. Confirm the connected project owner is merchant of record and that FFDB does
   not silently add an application fee.

Pass: no legacy account-type labels are used, charge types are not mixed, and merchant-of-record/liability behavior matches the documented configuration.

### PAY-06 — Webhooks, fulfillment, refunds, and retry safety

1. Send missing, malformed, stale, wrong-account, wrong-mode, and valid Stripe
   signatures against a BYO project's webhook route and the global Connect
   route. Verify the Connect endpoint secret before `event.account` routing,
   cross-account rejection, and that Connect events cannot use the BYO route.
2. Deliver valid checkout/payment/subscription/refund events out of order and
   more than once; reuse one event ID with changed bytes.
3. Mark an order fulfilled, replay the payment event, then issue partial and full
   refunds within the server-recorded payment amount.
4. Restart the API between provider success and local idempotency completion and
   retry the original API request.

Pass: only valid raw-body signatures mutate state; event ID plus payload hash
prevents conflicting replay; fulfillment is explicit and cannot be inferred from
a redirect; refunds cannot exceed captured funds; exact API retries recover one
logical provider object without duplicate charges or subscriptions.

### PAY-07 — Credential rotation and account rebinding

1. Configure BYO test credentials, create one Product and Price, then configure
   a rotated secret and webhook secret for the same Stripe account.
2. Confirm the old ciphertext is replaced, the response and logs contain no
   secret material, and the existing catalog still creates Checkout sessions.
3. Before creating commerce objects on a fresh project, switch BYO to Connect
   and Connect to BYO; inspect the account plus secret rows in one transaction.
4. After creating a provider-bound Product, Customer, order, or subscription,
   attempt to bind that project to a different Stripe account.

Pass: same-account credential rotation and mode changes preserve provider
bindings; Connect removes the project BYO secret atomically; BYO replaces both
encrypted secret fields atomically; and a different-account rebind is rejected
without changing the active account or leaking credentials.

### PAY-08 — Customer reuse and Customer Portal authorization

1. Complete recurring Checkout for an individual and deliver the verified
   `checkout.session.completed` event with a Stripe Customer ID.
2. Create a second recurring Checkout for the same subject and inspect the
   provider request fixture.
3. Create a Customer Portal session as that end user, as a different end user,
   and as a project owner/admin. Repeat for a team and organization subject.
4. Request a portal session before any verified Checkout has bound a Customer,
   with an HTTP return URL outside localhost, and with a fragment or credentials
   in the URL.

Pass: later Checkout uses the existing account-scoped Stripe Customer; an
individual can open only their own portal; team/organization subjects require
commerce administration; the portal URL is accepted only from Stripe over
HTTPS; and missing customer bindings or unsafe return URLs fail closed.

### PAY-09 — Catalog immutability and provider metadata ownership

1. Create products with 0, 50, and 51 metadata entries; try nested values,
   bracketed keys, overlong keys/values, and every `ffdb_` reserved prefix.
2. Create one-time and recurring Prices, then attempt to change currency,
   amount, interval, entitlement grants, or provider identifiers in place.
3. Retire a Price and archive a Product while historical order and subscription
   snapshots still reference them.

Pass: valid scalar metadata round-trips without internal provider fields;
reserved/nested/oversized metadata is rejected before a provider mutation;
Price monetary and entitlement terms are immutable; retired catalog items
cannot start new Checkout but remain readable in historical state.

### PAY-10 — API, SDK, CLI, portal, and OpenAPI parity

1. Compare every `/commerce` router path with `openapi.json`,
   `client.commerce`, CLI help, docs route tables, and the portal network log.
2. Exercise BYO/Connect configuration, account refresh, product/price lifecycle,
   order/payment/subscription listing, partial refund, immediate and period-end
   cancellation, Customer Portal, entitlements, and paid fulfillment through
   both the SDK and the management portal.
3. Run the CLI with human output and `--json`; verify secrets are read only from
   protected environment input and are never printed.

Pass: method, path, credential, request body, response shape, status code, and
idempotency requirements agree across every surface; the legacy project
payments summary remains compatible while full operations use `/commerce`.

## Landing, docs, and portal

### UI-01 — Truthful installation

Pass when the landing primary command and every installation page use the release bundle, source setup is labeled contributor-only, publication status is explicit, and all commands match shipped artifacts.

### UI-02 — Documentation depth

For every docs route, confirm it contains the problem/what, why/when, prerequisites, required values, end-to-end example, result/verification, failure modes/security, and next step. Empty optional headings do not count.

### UI-03 — Billing and Payments scope

Pass when landing/docs/portal consistently use “Platform billing” for organization FFDB charges and “Project payments” for application commerce, show disabled/unconfigured states honestly, and never present browser redirects as payment confirmation.
