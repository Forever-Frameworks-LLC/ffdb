# Self-hosting installation

FFDB's primary distribution is a versioned Compose bundle backed by
multi-architecture, digest-pinned container images. It installs and operates
without a repository checkout. An announced tag on the
`Forever-Frameworks-LLC/ffdb` GitHub Releases page is the canonical source for
the installer, release assets, checksums, and notes. The `latest/download` URLs
select the latest stable tag; production automation should pin an exact tag.

The repository's default `compose.yaml` remains a disposable development
environment. It contains local PostgreSQL, MinIO, Mailpit, and development
credentials and must not be exposed publicly.

## What a release contains

| Artifact | Purpose |
| --- | --- |
| `ffdb-compose-bundle-VERSION.tar.gz` | Recommended external-provider model, explicit single-host evaluation model, exact image digests, configuration template, controller, and scripts |
| `ffdb-host-VERSION` | Architecture-neutral POSIX lifecycle controller |
| `SHA256SUMS` and `SHA256SUMS.sigstore.json` | Mandatory asset digests and keyless Sigstore signature bundle |
| `release-manifest.json` | Machine-readable version, architecture, image digest, signer, native state-schema, rollback floor, and architecture asset metadata |
| `ffdb-native-linux-{amd64,arm64}-VERSION.tar.gz` | Advanced native Linux/systemd binaries, static web applications, units, and installers |
| `ffdb-host.rb` | Generated, checksum-pinned Homebrew formula candidate; not a published tap |

The Compose bundle does not contain executables or static web files. Those are
inside the signed `linux/amd64` and `linux/arm64` runtime and gateway images. The
bundle records images by immutable `sha256` manifest digest, never by a mutable
tag. Its signed metadata also pins the PostgreSQL, MinIO, and Mailpit images used
only by the single-host profile. The separate native archives are complete
architecture-specific artifacts.

## External production prerequisites

The Compose installation requires Linux or macOS on amd64 or arm64, Docker
Engine/Desktop with the Compose plugin, `curl`, and `tar`. Install `cosign` to
verify keyless signatures and use `--require-signature` to fail closed.

Provision these durable services before starting FFDB:

- a PostgreSQL 17-compatible database and dedicated TLS-enabled FFDB role with
  the schema migration privileges required by the embedded migrator;
- a private HTTPS S3-compatible bucket with an exact browser CORS allowlist;
- a Resend API key and verified sender;
- durable local/block storage for SQLite projects, encrypted backups, and the
  organization usage/billing ledger below `FFDB_METRICS_ROOT`;
- a DNS name, TLS-terminating reverse proxy, and off-host backup destination.

Keep these values stable in a secret manager: `FFDB_MASTER_KEY` and
`FFDB_BACKUP_MASTER_KEY` must be different 32-byte base64 keys;
`FFDB_CURSOR_HMAC_KEY` and `FFDB_BOOTSTRAP_TOKEN` must each contain at least 32
random characters; `FFDB_NODE_ID` must be a stable unique UUID. Generate them on
a trusted administrative machine. Key replacement after data exists is a
rotation operation; see [key rotation](key-rotation.md).

## Install the Compose distribution

### Recommended external-provider profile

The shortest bootstrap installs the stable bundle and a configuration template,
but deliberately does not start services while placeholders remain:

```sh
curl -fsSLo ffdb-install.sh \
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh
less ffdb-install.sh
sudo sh ffdb-install.sh --require-signature
sudoedit /etc/ffdb/ffdb.env
sudo ffdb-host start
```

For an auditable deployment, prepare a mode-`0600` production environment file,
pin an exact version, require signatures, and start only after validation:

```sh
curl -fsSLo /tmp/ffdb-install.sh \
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/download/v0.3.8/install.sh
sudo sh /tmp/ffdb-install.sh --version 0.3.8 \
  --release-base https://github.com/Forever-Frameworks-LLC/ffdb/releases/download/v0.3.8 \
  --env-file /secure/path/ffdb.env --start --require-signature
```

Replace `0.3.8` with the announced version you reviewed. Without `--version` or
`--tag`, the installer resolves `stable.txt` from the latest stable GitHub
Release. It downloads the release checksums, verifies their Sigstore bundle when
`cosign` is available, always verifies the bundle/controller SHA-256 digests,
and delegates installation to the verified controller. `--require-signature`
rejects a missing signature, missing `cosign`, or an invalid release/image
signature.

### One-command single-host evaluation profile

For a local evaluation, demo, or isolated small host, one explicit command starts
PostgreSQL, MinIO object storage, persistent Mailpit mail capture, the FFDB API
and workers, and the unified gateway:

```sh
curl -fsSLo ffdb-install.sh \
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh
less ffdb-install.sh
sudo sh ffdb-install.sh --profile single-host --start --require-signature
sudo ffdb-host status
curl --fail http://127.0.0.1:5173/readyz
```

The same selection can be written as `--profile single-host`. FFDB generates
independent PostgreSQL, MinIO, encryption, HMAC, bootstrap, and node credentials
with `openssl`, writes them to `/etc/ffdb/single-host.env` at mode `0600`, and
never prints them. Existing secrets are preserved on reinstall and upgrade.

The bootstrap token remains in that root-readable file; `status` does not render
the configuration. Treat service logs as sensitive. To copy only that value into
a root-owned file for ingestion by a secret manager without exposing it on the
terminal:

```sh
sudo sh -c 'umask 077; sed -n "s/^FFDB_BOOTSTRAP_TOKEN=//p" \
  /etc/ffdb/single-host.env > /root/ffdb-bootstrap-token'
```

The browser endpoints are loopback-only. Port `5173` may look like a Vite
default, but an installed release does not run Vite: the host binding forwards
to the compiled nginx gateway's container port `8080`. nginx serves immutable
landing/docs/portal assets and proxies API paths to the separate Axum service at
`api:8080` on the private Compose network. Axum is not published on host port
`8080` by either packaged Docker profile.

- FFDB landing/docs/portal/API gateway: `http://127.0.0.1:5173`;
- MinIO API and console: `http://127.0.0.1:9000` and `:9001`;
- Mailpit SMTP and captured-mail UI: `127.0.0.1:1025` and
  `http://127.0.0.1:8025`.

PostgreSQL has no host port. Seven named volumes retain PostgreSQL, object data,
captured mail, project SQLite files, encrypted backups, per-organization usage
and billing ledgers, and sync state. A normal stop, upgrade, rollback, or
uninstall preserves all seven.

The single-host profile is deliberately labeled evaluation: it uses local HTTP
object storage and SMTP capture, concentrates every durable dependency on one
failure domain, and does not provide public TLS or real email delivery. Do not
expose its ports or turn it into internet production by editing bindings. Use the
default `external` profile with independently backed-up PostgreSQL, HTTPS object
storage, Resend, and a TLS proxy for production.

An installation owns one profile at a time. The controller refuses an in-place
profile switch so it cannot orphan a profile's volumes or secrets. Back up first,
run the explicit `uninstall --purge-data --yes` flow if switching is intended,
then install the other profile.

A one-line bootstrap relies on HTTPS for the installer itself. For the strongest
bootstrap, download `install.sh`, `SHA256SUMS`, and
`SHA256SUMS.sigstore.json` from the same versioned release directory; verify the
checksum list with `cosign verify-blob` and the workflow identity recorded in the
release manifest; then verify `install.sh` against that list before executing it.

The default filesystem layout is:

- `/opt/ffdb/releases/VERSION`: immutable extracted release assets;
- `/opt/ffdb/current`: symlink selecting the active release;
- `/etc/ffdb/ffdb.env`: mode-`0600` production configuration;
- `/etc/ffdb/single-host.env`: generated evaluation secrets, when selected;
- `/etc/ffdb/install-profile`: active `external` or `single-host` profile;
- `/usr/local/bin/ffdb-host`: installed lifecycle controller.
- `/usr/local/bin/ffdb-backup`: version-matched recovery engine invoked by
  `ffdb-host backup` for the single-host profile.

An existing configuration is preserved on reinstall and upgrade unless
`--replace-config` is explicit. Edit every `replace-me` value and `example.com`
hostname. `FFDB_S3_PUBLIC_ORIGIN` is the exact origin used in the gateway CSP:
scheme, host, and optional port, with no path, query, quotes, or trailing slash.
The production model rejects unsafe HTTP provider endpoints, weak secrets, and
other development defaults at runtime.

## Operate, upgrade, and remove

Use the installed controller rather than invoking Compose with ad hoc project
names:

```sh
sudo ffdb-host status
sudo ffdb-host logs --tail 200 api sync-worker gateway
sudo ffdb-host verify
sudo ffdb-host backup create /secure/ffdb-host-2026-08-03.tar.gz
sudo ffdb-host stop
sudo ffdb-host start
```

`start` validates the selected Compose model, confirms every selected-profile
image reference is digest-pinned, verifies FFDB image signatures when `cosign`
is present (or required), pulls the images, and waits for the services to become
healthy. The gateway binds only to
`127.0.0.1:${FFDB_GATEWAY_PORT:-5173}`. PostgreSQL and S3 are not deployed by the
recommended external profile.

Before every single-host update, create a complete versioned host archive. The
command quiesces mutation services, captures PostgreSQL and every durable volume
at one recovery point, and resumes them automatically:

```sh
sudo ffdb-host backup create /secure/ffdb-before-update.tar.gz
```

For the external-provider profile, coordinate provider PostgreSQL/S3 backups and
the FFDB project/metrics/backup volumes while ingress is quiesced; the controller
refuses to label an incomplete provider-independent archive as complete. Check
for a stable update without changing the host, read its release notes, then
update:

```sh
sudo ffdb-host version
sudo ffdb-host update-check
sudo FFDB_REQUIRE_SIGNATURE=1 ffdb-host update

# Reproducible scheduled alternative:
sudo ffdb-host update-check --version 0.3.8
sudo FFDB_REQUIRE_SIGNATURE=1 ffdb-host update --version 0.3.8
```

The new release is installed beside prior releases and the same configuration
and named volumes are reused. The API and database worker always come from the
same runtime image. Mixed versions are unsupported.

Rollback only to an already-installed release and only after confirming its
binaries remain compatible with migrations already applied:

```sh
sudo ffdb-host rollback 0.3.1 --acknowledge-migration-risk
```

A normal uninstall removes containers and FFDB software while preserving the
selected profile's configuration and stable Compose volumes:

```sh
sudo ffdb-host uninstall
# Equivalent inspected HTTPS bootstrap from the stable GitHub Release:
curl -fsSLo ffdb-uninstall.sh \
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/uninstall.sh
less ffdb-uninstall.sh
sudo sh ffdb-uninstall.sh
```

Data deletion is separate and intentionally difficult to invoke accidentally:

```sh
sudo ffdb-host uninstall --purge-data --yes
```

That command irreversibly removes the Compose project's named volumes and FFDB
configuration. Normal stop, upgrade, rollback, and uninstall never pass
`docker compose down --volumes`. A confirmed host restore does replace volumes
only after complete archive validation. Never copy a live SQLite database or WAL
as a backup; use the FFDB project backup API or the complete
[backup/restore workflow](backup-restore.md).

## Mirrors, offline installation, and release validation

The installer supports a version directory on an HTTPS mirror or local
filesystem. This is also the supported way to exercise a release candidate
before its GitHub tag is published:

```sh
sudo FFDB_VERSION=0.3.8 \
  FFDB_RELEASE_BASE_URL=file:///srv/ffdb/releases/v0.3.8 \
  sh /srv/ffdb/releases/v0.3.8/install.sh \
  --env-file /secure/path/ffdb.env
```

For a GitHub Enterprise or remote mirror, set `FFDB_GITHUB_RELEASES_URL` to its
Releases root, or set `FFDB_RELEASE_BASE_URL` to the exact version directory.
Set `FFDB_STABLE_URL` when latest-version discovery is mirrored separately, and
pair an exact release base with `FFDB_VERSION`. Mirror
`SHA256SUMS.sigstore.json` unchanged so the original release workflow identity
remains verifiable.

Release engineers can build and test candidate artifacts from a verified source
checkout without publishing them:

```sh
make release-check
FFDB_VERSION=0.3.8 \
FFDB_RUNTIME_IMAGE=ghcr.io/example/ffdb-runtime@sha256:FULL_DIGEST \
FFDB_GATEWAY_IMAGE=ghcr.io/example/ffdb-gateway@sha256:FULL_DIGEST \
FFDB_POSTGRES_IMAGE=postgres:17.5-alpine@sha256:FULL_DIGEST \
FFDB_MINIO_IMAGE=minio/minio:RELEASE.2025-04-22T22-12-26Z@sha256:FULL_DIGEST \
FFDB_MAILPIT_IMAGE=axllent/mailpit:v1.27.8@sha256:FULL_DIGEST \
make release-bundle
```

Publication is not complete merely because the GitHub Release page opens in a
browser. A non-browser probe must follow `latest/download/stable.txt`, then fetch
`install.sh`, `uninstall.sh`, checksums, signatures, bundles, native archives,
and package tarballs from `download/vVERSION`. That probe is a release gate and
must succeed before release notes call the tag installable:

```sh
make distribution-check
```

The gate defaults to full verification. It rejects HTTP `403`, HTML challenge
pages, redirects to interactive challenges, and content-type/body mismatches. It
also verifies the canonical Sigstore identity for `SHA256SUMS`, both signed FFDB
images, every versioned checksum, both native architecture archives, all six SDK
packages, and reachability of all five digest-pinned images. Release engineers
therefore need `cosign` and either Docker Buildx or `crane`. Setting
`FFDB_DISTRIBUTION_REQUIRE_FULL_VERIFICATION=0` permits a limited asset-coherence
diagnostic when those tools are absent, but its output explicitly does not claim
the channel is installable and it must not be used as the publication gate.

This source workflow is for release engineering, not the end-user installation
path. Tag automation builds both architectures, signs immutable image manifests
and `SHA256SUMS`, creates the GitHub Release, and uploads every canonical asset.
The additional `distribution-site` artifact can seed a compatible mirror, but it
is not required for the GitHub installation path.

## Advanced native Linux bundle

Native bundles are an advanced Linux-only alternative for operators who accept
responsibility for systemd, Caddy, DNS, and distribution-library compatibility. They
do not require a checkout or local compiler. The host must provide `caddy`,
`systemd`, `curl`, `tar`, `cosign`, PostgreSQL client tools
(`pg_dump`/`pg_restore`), and `sqlite3`; the installer fails before mutation if
the backup or signed-update prerequisites are missing. Download the architecture archive,
`SHA256SUMS`, and `SHA256SUMS.sigstore.json` from the same version directory;
verify the signed checksum list and then the archive before extraction:

```sh
VERSION=0.3.8
RELEASE_BASE="https://github.com/Forever-Frameworks-LLC/ffdb/releases/download/v$VERSION"
curl -fsSLO "$RELEASE_BASE/SHA256SUMS"
curl -fsSLO "$RELEASE_BASE/SHA256SUMS.sigstore.json"
curl -fsSLO "$RELEASE_BASE/ffdb-native-linux-amd64-$VERSION.tar.gz"
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "https://github.com/Forever-Frameworks-LLC/ffdb/.github/workflows/release.yml@refs/tags/v$VERSION" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf "ffdb-native-linux-amd64-$VERSION.tar.gz"
cd "ffdb-native-$VERSION"
sudo ./install-native.sh --verified-release \
  --env-file /secure/path/ffdb.env --start
```

Choose the `arm64` archive on ARM64. The installer creates the `ffdb` account and
state directories, installs all three same-release binaries and static sites,
renders one Caddy gateway from `FFDB_PUBLIC_BASE_URL` and
`FFDB_S3_PUBLIC_ORIGIN`, and enables the hardened services only with `--start`.
Caddy serves the sites, obtains the public certificate, and proxies directly to
loopback Axum; nginx is not installed or placed in the request path. The
installer preserves `/etc/ffdb` and
`/var/lib/ffdb` by default, including the mode-`0700`
`/var/lib/ffdb/metrics` billing-ledger directory:

Use `--verified-release` only after the preceding Sigstore and SHA-256 checks
succeed. The flag records the canonical release-workflow identity on the
installed release so it remains eligible as a future rollback target.

```sh
sudo ffdb-backup create /secure/ffdb-native-2026-08-03.tar.gz
sudo ./uninstall-native.sh
# Irreversible:
sudo ./uninstall-native.sh --purge-data --yes
```

### Signed native updates from the portal

The native installer also installs `ffdb-update`, a root-owned systemd path
agent, and a periodic stable-release check. The unprivileged API may submit only
typed check, exact-version install, exact-version rollback, schedule, and job
lookups. It cannot provide a download URL, executable path, or shell text. The
agent verifies the canonical release manifest, keyless Sigstore identity, and
asset checksum before it creates a coordinated backup or changes the host.

```sh
sudo systemctl --no-pager --full status \
  ffdb-update-agent.path ffdb-update-check.timer
sudo -u ffdb /usr/local/bin/ffdb-update inspect
```

Open **Global administration → Updates** as an instance owner or administrator
to check the stable channel. Install, rollback, and schedule changes require a
platform session issued within 15 minutes; the portal reauthenticates through
the normal sign-in route and never sends a password to the updater. The portal
receives a persisted job ID before Axum restarts and reconnects through Caddy
until readiness returns.

Releases are installed side by side below `/opt/ffdb/releases/VERSION` and the
complete API, database worker, sync worker, units, and web assets are selected
by the atomic `/opt/ffdb/current` link. Rollback is limited to a previously
verified installed release with a compatible native state schema and rollback
floor. A rejected compatibility check requires the coordinated backup/restore
workflow; it is not an acknowledgement flag that an operator can bypass.

Automatic checks are enabled by default. Automatic application is disabled by
default and requires an explicit UTC maintenance window. A check outside that
window records availability without restarting anything. On failure, keep the
job ID and inspect the root service plus application journal before submitting
another operation:

```sh
sudo -u ffdb /usr/local/bin/ffdb-update job "$JOB_ID"
sudo journalctl -u ffdb-update-agent.service -u ffdb-api.service \
  -u ffdb-sync-worker.service --since today --no-pager
curl --fail http://127.0.0.1:8080/readyz
curl --fail http://127.0.0.1:5173/readyz
```

The Docker release controller remains host operated. FFDB intentionally does
not mount the Docker socket or a host root command boundary into the API
container solely to expose a portal update button; use signed `ffdb-host
update-check`, `update`, and compatible `rollback` there.

There is no public Homebrew tap, `.deb`, or `.rpm` today. A release-generated
Homebrew formula is intentionally controller-only and must not be advertised
until its versioned URL, checksum, tap review, and Docker Compose prerequisite
are all maintained. Distro packages are not scaffolded because they would create
a second native dependency and hardening lifecycle without current maintainers.

## Public TLS and acceptance

Point the configured public hostname at the server and permit inbound TCP 80 and
443 so Caddy can obtain and renew its certificate. The same process serves `/`,
`/docs/`, and `/app/`, then proxies `/v1`, `/healthz`, `/readyz`, and
`/openapi.json` directly to Axum on `127.0.0.1:8080`. Raw `/metrics` is rejected
at the gateway and remains available only from the loopback Axum listener. The
secondary `127.0.0.1:5173` Caddy listener is for on-host acceptance checks; it is
not another proxy stage.

After installation, verify readiness through the public hostname, direct reloads
under `/docs/` and `/app/`, browser S3 CORS/CSP, authentication, RLS isolation,
sync, backups, and a restore. See [production deployment](production-deployment.md),
[observability](observability.md), and [incident response](incident-response.md)
before admitting real data.
