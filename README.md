# FFDB

FFDB is a self-hostable data platform that combines a PostgreSQL control plane
with one security-hardened SQLite application database per project. It provides
PostgreSQL-style row-level policies, authentication, logical offline sync,
RLS-protected S3-compatible object storage, a typed SDK/CLI, and a management
portal without exposing raw SQLite files.

Security-sensitive changes are accepted only with their release-checklist tests;
see [the implementation plan](docs/implementation/plan.md) and
[threat model](docs/threat-model/threat-model.md).

FFDB is designed to be installed from a versioned release bundle. Operators do
not need Rust, Node.js, pnpm, or a source checkout: the bundle contains the
digest-pinned Compose definition, configuration template, and the `ffdb-host`
lifecycle command; signed container images contain the services and web apps.

Tagged GitHub Releases are the canonical server distribution: each tag carries
the installer, signed checksums, digest-pinned Compose bundle, native archives,
and integration-package tarballs. Use the stable `latest/download` installer
only after a release is announced, or pin the exact tag for reproducible
automation. All six JavaScript packages publish under the `@ffdb` npm scope
with provenance. Pin every package to the server version; the same release also
provides checksum-listed tarballs for verified offline installation.

## Repository

- `apps/api`: Rust HTTP/control-plane service
- `apps/database-worker`: isolated SQLite worker
- `apps/sync-worker`: asynchronous sync/maintenance worker
- `apps/landing`: public React/Vite landing site
- `apps/docs`: React/Vite documentation application
- `apps/portal`: React/Vite management portal
- `examples/field-notes`: polished React + Node feature lab for the public FFDB application surface
- `crates`: narrow Rust security, data, and provider components
- `packages`: TypeScript client, React/RN integrations, sync client, CLI, and email components
- `infra`: local PostgreSQL and deployment assets
- `docs`: architecture, security, protocol, and operations manuals

## Install and host FFDB

A new Linux host can start the complete loopback-only evaluation product and all
of its dependencies from the latest announced stable GitHub Release with:

```bash
curl -fsSLo ffdb-install.sh \
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh
less ffdb-install.sh
sudo sh ffdb-install.sh --profile single-host --start --require-signature
sudo ffdb-host status
# Compiled nginx gateway readiness; this is not Vite.
curl --fail http://127.0.0.1:5173/readyz
```

The `single-host` profile generates strong local secrets without printing them
and starts PostgreSQL, MinIO, persistent Mailpit capture, FFDB services, and the
web gateway. The installer puts immutable releases under `/opt/ffdb`, operator
configuration under `/etc/ffdb`, and leaves durable volumes intact across
upgrades and normal uninstall operations. `ffdb-host` also provides `stop`,
`logs`, `verify`, complete `backup create`/`backup restore`, `update-check`, `update`,
`rollback`, and `uninstall` commands.

Create a complete single-host recovery point without checking out the repository:

```sh
sudo ffdb-host backup create /secure/ffdb-host-2026-08-03.tar.gz
# Restore is destructive, requires the exact FFDB version, and refuses a running host.
sudo ffdb-host stop
sudo ffdb-host backup restore /secure/ffdb-host-2026-08-03.tar.gz --yes
```

Port `5173` is the loopback host port for the packaged **compiled nginx
gateway**, not a Vite development server. The gateway image contains immutable
production builds of the landing site, documentation, and portal; it serves
those files and proxies `/v1`, `/healthz`, `/readyz`, and `/openapi.json` to the
Axum API on the private Compose network at `api:8080`. Raw `/metrics` remains
private to the API listener; the portal uses the authenticated observability API.
The packaged Docker profiles do not publish the Axum container's port `8080` to
the host, and no Vite process runs in an installed release.

This profile is for evaluation, demos, and isolated hosts. It uses development
mode, loopback HTTP, and captured email; do not expose it to the internet. For
production, use the default external-provider profile with independently backed
up PostgreSQL, HTTPS object storage, real email delivery, and TLS:

```bash
curl -fsSLo ffdb-install.sh \
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh
less ffdb-install.sh
sudo sh ffdb-install.sh --require-signature
sudoedit /etc/ffdb/ffdb.env
sudo ffdb-host start
sudo ffdb-host status
# Compiled nginx gateway readiness; this is not Vite.
curl --fail http://127.0.0.1:5173/readyz
```

The single-host bootstrap token remains only in the root-readable
`/etc/ffdb/single-host.env`; status does not render the configuration. Treat
service logs as sensitive. To hand the token to a secret manager without writing
it to the terminal, extract it into a root-only file:

```bash
sudo sh -c 'umask 077; sed -n "s/^FFDB_BOOTSTRAP_TOKEN=//p" \
  /etc/ffdb/single-host.env > /root/ffdb-bootstrap-token'
```

For a reproducible rollout, download the installer from an exact announced tag
and pass the same version explicitly:

```bash
VERSION=0.3.11
RELEASE_BASE="https://github.com/Forever-Frameworks-LLC/ffdb/releases/download/v$VERSION"
curl -fsSLo ffdb-install.sh "$RELEASE_BASE/install.sh"
sudo sh ffdb-install.sh --version "$VERSION" \
  --release-base "$RELEASE_BASE" --require-signature
sudoedit /etc/ffdb/ffdb.env
sudo ffdb-host start
```

Check for and apply later stable updates through the installed controller. Back
up first and read the target release notes; production automation can pass
`--version VERSION` to both commands instead of following latest:

```bash
sudo ffdb-host update-check
sudo ffdb-host backup create /secure/ffdb-before-update.tar.gz
sudo FFDB_REQUIRE_SIGNATURE=1 ffdb-host update
```

Read the [Docker release-bundle guide](apps/docs/src/content.ts) or the
[native systemd operations guide](docs/operations/self-hosting.md) before
exposing a deployment to the internet.

## SDK and CLI packages

The TypeScript SDK, sync client, React/React Native integrations, email
components, and CLI are version-matched public npm packages:

```bash
VERSION=0.3.11
npm install --save-exact \
  "@ffdb/client@$VERSION" \
  "@ffdb/sync-client@$VERSION" \
  "@ffdb/react@$VERSION"
npm install --global "@ffdb/cli@$VERSION"
ffdb --help
```

For offline or controlled-network installation, verify the matching GitHub
Release's `SHA256SUMS` and Sigstore bundle and install its six `.tgz` assets.
Never mix package versions or use an unpinned registry range in production.

These packages connect applications and operators to an FFDB host; they do not
install PostgreSQL, object storage, TLS, or the server runtime. The complete
server installation remains the release bundle above.

## Contributor quick start

Cloning the repository is for development and release engineering. Install Rust
1.96.1, Node 24+, pnpm 11.6, Docker, and Compose, then use the Makefile as the
single entry point:

```bash
make bootstrap
make build
make verify
make compose-rebuild
make status
# Public-style request through the compiled nginx gateway.
curl --fail http://localhost:5173/readyz
# Contributor-only direct Axum diagnostic, bypassing nginx.
curl --fail http://localhost:8080/readyz
```

`make build` compiles every Rust and TypeScript target. `make verify` runs the
complete formatting, lint, documentation, release-distribution, test, and build
suite. Neither command is part of the operator installation path.

`make compose-rebuild` builds the Rust services and a unified static nginx web
gateway from the current checkout, force-recreates their containers, and waits
for the stack. Vite runs only during the gateway image's build stage; the
running image contains the landing, docs, and portal production files and runs
nginx. Named PostgreSQL, MinIO, Mailpit, project, backup, organization-metrics,
and sync volumes are retained.
For a deliberate first-install acceptance run, use the guarded destructive
target below. It deletes only this Compose project's named volumes, rebuilds the
current checkout, and returns `/app/` to the owner-creation screen:

```bash
FFDB_CONFIRM_FRESH=DELETE_LOCAL_FFDB_DATA make compose-fresh
```

Use `make clean` to stop containers and remove only reproducible build outputs;
it does not remove local data or Docker volumes.

The compiled nginx gateway is at `http://localhost:5173`: landing is `/`,
documentation is `/docs/`, the management portal is `/app/`, and the gateway
proxies the API and OpenAPI routes to Axum. The contributor Compose file also
publishes Axum directly at `http://localhost:8080` for local backend diagnostics;
that direct port is not part of either packaged Docker profile. Captured mail is
at `http://localhost:8025`, and the public-style OpenAPI contract is at
`http://localhost:5173/openapi.json`. See
[local development](docs/operations/local-development.md) for migrations,
service diagnostics, test commands, and data-safety notes.

After the images are healthy, run the complete public-surface example and live
verification suite:

```bash
make live
```

Unlike invoking `pnpm test:live` directly, `make live` first builds every host
TypeScript workspace—including all three Vite apps—and rebuilds the running
Compose services and unified gateway from current sources, preventing the
harness from silently exercising stale images.

It creates an isolated organization/project, compiles RLS policies, registers
and verifies multiple users through Mailpit, proves different query results,
syncs an offline replica, performs protected S3 uploads, delivers a customized
template, and tests backup/restore. It does not print bearer credentials.

## Self-hosting

The release bundle is the default self-hosted deployment. Native Linux/systemd
artifacts are an advanced component path for operators who supply and harden
the surrounding services themselves; see the
[self-hosting guide](docs/operations/self-hosting.md). The repository's default
`compose.yaml` remains contributor tooling and is not a production template.

Native installations include a constrained, root-owned release updater. It
verifies canonical signed artifacts, creates a coordinated backup, installs
complete releases side by side below `/opt/ffdb/releases`, and switches
`/opt/ffdb/current` atomically. Instance owners and administrators can operate
that lifecycle from **Global administration → Updates** after a recent login;
automatic release checks are enabled, while automatic application stays off
until an owner explicitly configures a UTC maintenance window. The API never
receives sudo or general shell authority. Packaged Docker installations retain
the host-controlled `ffdb-host update` and `rollback` workflow rather than
mounting the Docker host or root socket into the API container.

## Security

Raw SQLite files and unprotected connections are never part of the public API.
End-user SQL is restricted by server parsing, SQLite preparation, an authorizer,
resource limits, and generated RLS machinery in an isolated worker. Read the
[threat model](docs/threat-model/threat-model.md) and [security policy](SECURITY.md)
before deploying or contributing to a trust-boundary component.

Licensed under Apache-2.0.
