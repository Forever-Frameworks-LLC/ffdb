#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-release-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
digest_a=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
digest_b=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
digest_c=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
digest_d=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
digest_e=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
digest_f=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
digest_1=1111111111111111111111111111111111111111111111111111111111111111
postgres_image="postgres:17.5-alpine@sha256:$digest_e"
minio_image="minio/minio:RELEASE.2025-04-22T22-12-26Z@sha256:$digest_f"
mailpit_image="axllent/mailpit:v1.27.8@sha256:$digest_1"
github_repository=Forever-Frameworks-LLC/ffdb
github_releases_url=https://github.test/$github_repository/releases

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

release_one=$test_root/release-0.1.0
release_two=$test_root/release-0.2.0
native_inputs=$test_root/native-inputs
native_output=$test_root/native-output
image_input=$test_root/image-input
extra_output=$test_root/extra-output
install -d "$release_one" "$release_two" "$test_root/fake-bin" \
  "$native_inputs/bin" "$native_inputs/web/docs" "$native_inputs/web/app" \
  "$native_output" "$extra_output"
for binary in ffdb-api ffdb-database-worker ffdb-sync-worker; do
  printf '#!/bin/sh\nexit 0\n' > "$native_inputs/bin/$binary"
  chmod 0755 "$native_inputs/bin/$binary"
done
for page in index.html docs/index.html app/index.html; do
  printf '<!doctype html><title>FFDB release test</title>\n' > "$native_inputs/web/$page"
done
SOURCE_DATE_EPOCH=1 "$ROOT_DIR/scripts/build-native-bundle.sh" 0.1.0 amd64 \
  "$native_inputs/bin" "$native_inputs/web" "$native_output"
SOURCE_DATE_EPOCH=1 "$ROOT_DIR/scripts/build-native-bundle.sh" 0.1.0 arm64 \
  "$native_inputs/bin" "$native_inputs/web" "$native_output"
"$ROOT_DIR/scripts/prepare-release-image-inputs.sh" 0.1.0 \
  "$native_output" "$image_input"
for arch in amd64 arm64; do
  for binary in ffdb-api ffdb-database-worker ffdb-sync-worker; do
    test -x "$image_input/runtime/$arch/$binary"
  done
done
for page in index.html docs/index.html app/index.html; do
  test -f "$image_input/web/$page"
done
cp "$native_output"/* "$extra_output/"
for package in client sync-client react react-native email-components cli; do
  package_root=$test_root/package-$package
  install -d "$package_root/package"
  case "$package" in
    client) package_name=@ffdb/client ;;
    cli) package_name=@ffdb/cli ;;
    *) package_name=@ffdb/$package ;;
  esac
  printf '{"name": "%s", "version": "0.1.0"}\n' "$package_name" \
    > "$package_root/package/package.json"
  tar -czf "$extra_output/ffdb-$package-0.1.0.tgz" -C "$package_root" package
done
: > "$extra_output/SDK-SHA256SUMS"
for package in client sync-client react react-native email-components cli; do
  asset=ffdb-$package-0.1.0.tgz
  printf '%s  %s\n' "$(file_sha256 "$extra_output/$asset")" "$asset" \
    >> "$extra_output/SDK-SHA256SUMS"
done
tar -tzf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  | grep -q 'ffdb-native-0.1.0/systemd/ffdb-api.service'
tar -tzf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  | grep -q 'ffdb-native-0.1.0/systemd/ffdb-gateway.Caddyfile'
tar -tzf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  | grep -q 'ffdb-native-0.1.0/systemd/ffdb-gateway.service'
tar -tzf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  | grep -q 'ffdb-native-0.1.0/web/app/index.html'
tar -tzf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  > "$test_root/native-archive.list"
grep -q 'ffdb-native-0.1.0/ffdb-backup' "$test_root/native-archive.list"
grep -q 'ffdb-native-0.1.0/bin/ffdb-update' "$test_root/native-archive.list"
grep -q 'ffdb-native-0.1.0/COMPATIBILITY' "$test_root/native-archive.list"
tar -xOf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  'ffdb-native-0.1.0/ffdb-backup' | grep -F -q 'BACKUP_TOOL_VERSION="0.1.0"'
tar -xOf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  'ffdb-native-0.1.0/COMPATIBILITY' \
  | grep -F -q 'FFDB_NATIVE_STATE_SCHEMA=1'
tar -xOf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  'ffdb-native-0.1.0/COMPATIBILITY' \
  | grep -F -q 'FFDB_NATIVE_MINIMUM_ROLLBACK_VERSION=0.3.0'
tar -xOf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  'ffdb-native-0.1.0/systemd/ffdb.env.example' \
  | grep -F -q 'FFDB_METRICS_ROOT=/var/lib/ffdb/metrics'
tar -xOf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  'ffdb-native-0.1.0/systemd/ffdb.tmpfiles.conf' \
  | grep -F -q 'd /var/lib/ffdb/metrics 0700 ffdb ffdb -'
tar -xOf "$native_output/ffdb-native-linux-amd64-0.1.0.tar.gz" \
  'ffdb-native-0.1.0/systemd/ffdb-api.service' \
  | grep -F -q 'ReadWritePaths=/var/lib/ffdb/projects /var/lib/ffdb/backups /var/lib/ffdb/metrics'
SOURCE_DATE_EPOCH=1 FFDB_EXTRA_ASSETS_DIR="$extra_output" \
  FFDB_GITHUB_REPOSITORY="$github_repository" \
  FFDB_GITHUB_RELEASES_URL="$github_releases_url" \
  FFDB_POSTGRES_IMAGE="$postgres_image" FFDB_MINIO_IMAGE="$minio_image" \
  FFDB_MAILPIT_IMAGE="$mailpit_image" \
  "$ROOT_DIR/scripts/build-release-bundle.sh" 0.1.0 \
  "registry.example/ffdb-runtime@sha256:$digest_a" \
  "registry.example/ffdb-gateway@sha256:$digest_b" "$release_one"
SOURCE_DATE_EPOCH=2 FFDB_GITHUB_REPOSITORY="$github_repository" \
  FFDB_GITHUB_RELEASES_URL="$github_releases_url" \
  FFDB_POSTGRES_IMAGE="$postgres_image" \
  FFDB_MINIO_IMAGE="$minio_image" FFDB_MAILPIT_IMAGE="$mailpit_image" \
  "$ROOT_DIR/scripts/build-release-bundle.sh" 0.2.0 \
  "registry.example/ffdb-runtime@sha256:$digest_c" \
  "registry.example/ffdb-gateway@sha256:$digest_d" "$release_two"

for release_dir in "$release_one" "$release_two"; do
  version=$(sed -n '1p' "$release_dir/stable.txt")
  grep -q "ffdb-compose-bundle-$version.tar.gz" "$release_dir/SHA256SUMS"
  for startup_asset in install.sh uninstall.sh "ffdb-host-$version" stable.txt; do
    grep -Eq "^[0-9a-f]{64}  $startup_asset$" "$release_dir/SHA256SUMS"
  done
  ! grep -R -E -q '@(VERSION|RUNTIME_IMAGE|GATEWAY_IMAGE|POSTGRES_IMAGE|MINIO_IMAGE|MAILPIT_IMAGE|SIGNATURE_IDENTITY|SHA256|ASSET_URL)@' "$release_dir"
  node - "$release_dir/release-manifest.json" "$version" \
    "$github_repository" "$github_releases_url/download/v$version" <<'NODE'
const { readFileSync } = require("node:fs");
const [path, version, repository, releaseUrl] = process.argv.slice(2);
const manifest = JSON.parse(readFileSync(path, "utf8"));
if (manifest.schema_version !== 2
    || manifest.version !== version
    || manifest.release_tag !== `v${version}`
    || manifest.github_repository !== repository
    || manifest.github_release_url !== releaseUrl
    || manifest.native_update?.state_schema !== 1
    || manifest.native_update?.minimum_upgrade_version !== "0.3.0"
    || manifest.native_update?.minimum_rollback_version !== "0.3.0"
    || manifest.native_update?.assets?.amd64 !== `ffdb-native-linux-amd64-${version}.tar.gz`
    || manifest.native_update?.assets?.arm64 !== `ffdb-native-linux-arm64-${version}.tar.gz`) {
  throw new Error("release manifest GitHub metadata is inconsistent");
}
NODE
  tar -xOf "$release_dir/ffdb-compose-bundle-$version.tar.gz" \
    "ffdb-$version/release.env" | awk -F= '
      /^FFDB_(RUNTIME|GATEWAY|POSTGRES|MINIO|MAILPIT)_IMAGE=/ {
        count++
        if ($2 !~ /@sha256:[0-9a-f]{64}$/) bad=1
      }
      END {exit bad || count != 5}
    '
  tar -tzf "$release_dir/ffdb-compose-bundle-$version.tar.gz" \
    > "$test_root/compose-$version.list"
  grep -q "ffdb-$version/ffdb-backup" "$test_root/compose-$version.list"
done
grep -F -q "$github_releases_url/download/v0.1.0/ffdb-host-0.1.0" \
  "$release_one/ffdb-host.rb"
for compose_file in compose.yaml compose.single-host.yaml; do
  tar -xOf "$release_one/ffdb-compose-bundle-0.1.0.tar.gz" \
    "ffdb-0.1.0/$compose_file" > "$test_root/extracted-$compose_file"
  grep -F -q 'FFDB_METRICS_ROOT: /var/lib/ffdb/metrics' "$test_root/extracted-$compose_file"
  grep -F -q 'metrics-data:/var/lib/ffdb/metrics' "$test_root/extracted-$compose_file"
done
for compose_file in compose.yaml compose.production.yaml \
  infra/release/compose.yaml infra/release/compose.single-host.yaml; do
  grep -F -q 'FFDB_METRICS_ROOT: /var/lib/ffdb/metrics' "$ROOT_DIR/$compose_file"
  grep -F -q 'metrics-data:/var/lib/ffdb/metrics' "$ROOT_DIR/$compose_file"
  grep -F -q 'metrics-data:' "$ROOT_DIR/$compose_file"
done
grep -F -q '/var/lib/ffdb/metrics' "$ROOT_DIR/infra/docker/Dockerfile.rust"
grep -E -q 'apt-get install .*sqlite3' "$ROOT_DIR/infra/docker/Dockerfile.rust"
grep -F -q 'dist/image-input/runtime/${TARGETARCH}/ffdb-api' \
  "$ROOT_DIR/infra/docker/Dockerfile.rust.release"
grep -F -q 'dist/image-input/web' "$ROOT_DIR/infra/docker/Dockerfile.portal.release"
grep -F -q 'needs: [validate, native]' "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q 'file: infra/docker/Dockerfile.rust.release' \
  "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q 'file: infra/docker/Dockerfile.portal.release' \
  "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q "printf 'FFDB_POSTGRES_IMAGE=%s\\n'" \
  "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q 'postgres_image=$(scripts/resolve-release-image.sh postgres:17.5-alpine)' \
  "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q '} >> "$GITHUB_ENV"' "$ROOT_DIR/.github/workflows/release.yml"
if grep -F -q 'needs.images.outputs.postgres_image' \
  "$ROOT_DIR/.github/workflows/release.yml"; then
  printf '%s\n' "infrastructure image references must not cross a job-output boundary" >&2
  exit 1
fi
grep -F -q 'Create or resume draft GitHub release' "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q 'gh release create "$GITHUB_REF_NAME"' "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q -- '--verify-tag' "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q 'gh release upload "$GITHUB_REF_NAME" dist/release/* --clobber' \
  "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q 'gh release edit "$GITHUB_REF_NAME" --draft=false' \
  "$ROOT_DIR/.github/workflows/release.yml"
if grep -F -q 'softprops/action-gh-release' "$ROOT_DIR/.github/workflows/release.yml"; then
  printf '%s\n' "release workflow must publish a complete draft before immutability" >&2
  exit 1
fi
if grep -E -q 'uses:[[:space:]]+[^[:space:]]+@(v[0-9]+|stable|nightly)([[:space:]]|$)' \
  "$ROOT_DIR/.github/workflows/ci.yml" "$ROOT_DIR/.github/workflows/release.yml"; then
  printf '%s\n' "GitHub Actions must be pinned to immutable commit SHAs" >&2
  exit 1
fi
grep -F -q 'npm install --global npm@12.0.2' "$ROOT_DIR/.github/workflows/release.yml"
if grep -E -q 'NPM_TOKEN|NODE_AUTH_TOKEN' "$ROOT_DIR/.github/workflows/release.yml"; then
  printf '%s\n' "npm releases must use trusted publishing instead of a long-lived token" >&2
  exit 1
fi
grep -F -q 'verify-github-release:' "$ROOT_DIR/.github/workflows/release.yml"
grep -F -q 'for package in client sync-client react react-native email-components cli; do' \
  "$ROOT_DIR/.github/workflows/release.yml"
for backup_prerequisite in pg_dump pg_restore sqlite3 curl tar; do
  grep -F -q "$backup_prerequisite" "$ROOT_DIR/infra/release/native/install-native.sh"
done
if grep -E -q 'FFDB_STRIPE_(PAYG|PRO)_PRICE_ID' \
  "$ROOT_DIR/docs/billing/README.md" "$ROOT_DIR/.env.example" \
  "$ROOT_DIR/infra/release/ffdb.env.example" \
  "$ROOT_DIR/infra/docker/production.env.example" \
  "$ROOT_DIR/infra/systemd/ffdb.env.example"; then
  printf '%s\n' "obsolete aggregate Stripe Price ID remains in configuration documentation" >&2
  exit 1
fi
for required_billing_key in FFDB_STRIPE_SECRET_KEY FFDB_STRIPE_WEBHOOK_SECRET \
  FFDB_STRIPE_PRO_BASE_PRICE_ID \
  FFDB_STRIPE_READS_EVENT_NAME FFDB_STRIPE_READS_METER_ID \
  FFDB_STRIPE_PAYG_READS_PRICE_ID FFDB_STRIPE_PRO_READS_PRICE_ID \
  FFDB_STRIPE_WRITES_EVENT_NAME FFDB_STRIPE_WRITES_METER_ID \
  FFDB_STRIPE_PAYG_WRITES_PRICE_ID FFDB_STRIPE_PRO_WRITES_PRICE_ID \
  FFDB_STRIPE_STORAGE_EVENT_NAME FFDB_STRIPE_STORAGE_METER_ID \
  FFDB_STRIPE_PAYG_STORAGE_PRICE_ID FFDB_STRIPE_PRO_STORAGE_PRICE_ID \
  FFDB_STRIPE_MAU_EVENT_NAME FFDB_STRIPE_MAU_METER_ID \
  FFDB_STRIPE_PAYG_MAU_PRICE_ID FFDB_STRIPE_PRO_MAU_PRICE_ID \
  FFDB_STRIPE_PRO_BILLING_UNIT \
  FFDB_BILLING_SUCCESS_URL FFDB_BILLING_CANCEL_URL FFDB_BILLING_PORTAL_RETURN_URL; do
  grep -F -q "$required_billing_key" "$ROOT_DIR/docs/billing/README.md"
  for compose_file in compose.yaml compose.production.yaml \
    infra/release/compose.yaml infra/release/compose.single-host.yaml; do
    grep -F -q "$required_billing_key" "$ROOT_DIR/$compose_file"
  done
  for environment_example in .env.example infra/release/ffdb.env.example \
    infra/docker/production.env.example infra/systemd/ffdb.env.example; do
    grep -F -q "$required_billing_key" "$ROOT_DIR/$environment_example"
  done
done
for required_connect_key in \
  FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY \
  FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET \
  FFDB_COMMERCE_STRIPE_CONNECT_SECRET_KEY \
  FFDB_COMMERCE_STRIPE_CONNECT_WEBHOOK_SECRET; do
  grep -F -q "$required_connect_key" "$ROOT_DIR/docs/billing/README.md"
  for compose_file in compose.yaml compose.production.yaml \
    infra/release/compose.yaml infra/release/compose.single-host.yaml; do
    grep -F -q "$required_connect_key" "$ROOT_DIR/$compose_file"
  done
  for environment_example in .env.example infra/release/ffdb.env.example \
    infra/docker/production.env.example infra/systemd/ffdb.env.example; do
    grep -F -q "$required_connect_key" "$ROOT_DIR/$environment_example"
  done
done
grep -F -q 'FFDB_STRIPE_STORAGE_EVENT_NAME=ffdb_storage_kilobyte_hours' \
  "$ROOT_DIR/docs/billing/README.md"
grep -q 'ffdb-native-linux-amd64-0.1.0.tar.gz' "$release_one/SHA256SUMS"
grep -q 'SDK-SHA256SUMS' "$release_one/SHA256SUMS"
for package in client sync-client react react-native email-components cli; do
  grep -q "ffdb-$package-0.1.0.tgz" "$release_one/SHA256SUMS"
done
if command -v ruby >/dev/null 2>&1; then
  ruby -c "$release_one/ffdb-host.rb" >/dev/null
fi

install -d "$test_root/rendered"
tar -xzf "$release_one/ffdb-compose-bundle-0.1.0.tar.gz" -C "$test_root/rendered"
[ -f "$test_root/rendered/ffdb-0.1.0/compose.single-host.yaml" ]

cat > "$test_root/fake-bin/docker" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "${FFDB_TEST_DOCKER_LOG:?}"
[ "${1:-}" = compose ] || exit 1
exit 0
EOF
chmod 0755 "$test_root/fake-bin/docker"

# Keep controller tests isolated from a developer workstation's Cosign install
# and from the network. Public release signature behavior is exercised below
# with a separate logging fake that validates the expected CLI arguments.
cat > "$test_root/fake-bin/cosign" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 0755 "$test_root/fake-bin/cosign"

sed \
  -e 's|https://ffdb.example.com|https://ffdb.test|g' \
  -e 's|postgres://ffdb:replace-me@postgres.example.com:5432/ffdb?sslmode=require|postgres://ffdb:password@postgres.test:5432/ffdb?sslmode=require|' \
  -e 's|replace-me-with-32-bytes-of-base64|AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=|' \
  -e 's|replace-me-with-an-independent-32-byte-base64-key|AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=|' \
  -e 's|replace-me-with-at-least-32-random-characters|0123456789abcdef0123456789abcdef|g' \
  -e 's|https://s3.example.com|https://s3.test|g' \
  -e 's|FFDB_S3_ACCESS_KEY_ID=replace-me|FFDB_S3_ACCESS_KEY_ID=test-access|' \
  -e 's|FFDB_S3_SECRET_ACCESS_KEY=replace-me|FFDB_S3_SECRET_ACCESS_KEY=test-secret|' \
  -e 's|FFDB_RESEND_API_KEY=re_replace_me|FFDB_RESEND_API_KEY=re_0123456789abcdef|' \
  -e 's|noreply@example.com|noreply@ffdb.test|' \
  "$ROOT_DIR/infra/release/ffdb.env.example" > "$test_root/config.env"

docker_path=$(command -v docker)
"$docker_path" compose \
  --env-file "$test_root/rendered/ffdb-0.1.0/release.env" \
  --env-file "$test_root/config.env" \
  -f "$test_root/rendered/ffdb-0.1.0/compose.yaml" config --quiet

export FFDB_ALLOW_UNPRIVILEGED=1
export FFDB_INSTALL_ROOT=$test_root/opt/ffdb
export FFDB_CONFIG_DIR=$test_root/etc/ffdb
export FFDB_CONFIG_FILE=$FFDB_CONFIG_DIR/ffdb.env
export FFDB_BIN_DIR=$test_root/bin
export FFDB_TEST_DOCKER_LOG=$test_root/docker.log
export PATH=$test_root/fake-bin:$PATH

FFDB_VERSION=0.1.0 FFDB_RELEASE_BASE_URL="file://$release_one" \
  sh "$ROOT_DIR/infra/release/install.sh" --env-file "$test_root/config.env" --start
[ "$(readlink "$FFDB_INSTALL_ROOT/current")" = "$FFDB_INSTALL_ROOT/releases/0.1.0" ]
[ -x "$FFDB_BIN_DIR/ffdb-host" ]
[ -x "$FFDB_BIN_DIR/ffdb-backup" ]
"$FFDB_BIN_DIR/ffdb-host" help | grep -F -q 'backup     Create or restore'
[ -f "$FFDB_CONFIG_FILE" ]
"$FFDB_BIN_DIR/ffdb-host" status >/dev/null
"$FFDB_BIN_DIR/ffdb-host" stop >/dev/null

FFDB_RELEASE_BASE_URL="file://$release_two" "$FFDB_BIN_DIR/ffdb-host" upgrade 0.2.0
[ "$(readlink "$FFDB_INSTALL_ROOT/current")" = "$FFDB_INSTALL_ROOT/releases/0.2.0" ]
"$FFDB_BIN_DIR/ffdb-host" rollback 0.1.0 --acknowledge-migration-risk
[ "$(readlink "$FFDB_INSTALL_ROOT/current")" = "$FFDB_INSTALL_ROOT/releases/0.1.0" ]

"$FFDB_BIN_DIR/ffdb-host" uninstall
[ -f "$FFDB_CONFIG_FILE" ]
[ ! -e "$FFDB_INSTALL_ROOT/current" ]
[ ! -e "$FFDB_BIN_DIR/ffdb-backup" ]
grep -q 'down --remove-orphans' "$FFDB_TEST_DOCKER_LOG"

FFDB_VERSION=0.1.0 FFDB_RELEASE_BASE_URL="file://$release_one" \
  sh "$ROOT_DIR/infra/release/install.sh"
"$FFDB_BIN_DIR/ffdb-host" uninstall --purge-data --yes
[ ! -e "$FFDB_CONFIG_DIR" ]
grep -q 'down --remove-orphans --volumes' "$FFDB_TEST_DOCKER_LOG"

tampered=$test_root/tampered
cp -R "$release_one" "$tampered"
printf 'tamper\n' >> "$tampered/ffdb-host-0.1.0"
if FFDB_VERSION=0.1.0 FFDB_RELEASE_BASE_URL="file://$tampered" \
  sh "$ROOT_DIR/infra/release/install.sh" >/dev/null 2>&1; then
  printf '%s\n' "tampered release unexpectedly installed" >&2
  exit 1
fi

: > "$FFDB_TEST_DOCKER_LOG"
single_log=$test_root/single-host-install.log
FFDB_VERSION=0.1.0 FFDB_RELEASE_BASE_URL="file://$release_one" \
  sh "$ROOT_DIR/infra/release/install.sh" --profile single-host --start > "$single_log"
single_config=$FFDB_CONFIG_DIR/single-host.env
[ -f "$single_config" ]
[ "$(sed -n '1p' "$FFDB_CONFIG_DIR/install-profile")" = single-host ]
if [ "$(uname -s)" = Darwin ]; then
  [ "$(stat -f '%Lp' "$single_config")" = 600 ]
else
  [ "$(stat -c '%a' "$single_config")" = 600 ]
fi
master_key=$(sed -n 's/^FFDB_MASTER_KEY=//p' "$single_config")
backup_key=$(sed -n 's/^FFDB_BACKUP_MASTER_KEY=//p' "$single_config")
[ -n "$master_key" ] && [ -n "$backup_key" ] && [ "$master_key" != "$backup_key" ]
[ "$(printf '%s' "$master_key" | openssl base64 -d -A | wc -c | tr -d ' ')" = 32 ]
[ "$(printf '%s' "$backup_key" | openssl base64 -d -A | wc -c | tr -d ' ')" = 32 ]
for secret_key in FFDB_SINGLE_HOST_POSTGRES_PASSWORD FFDB_SINGLE_HOST_MINIO_SECRET_KEY \
  FFDB_CURSOR_HMAC_KEY FFDB_BOOTSTRAP_TOKEN; do
  secret_value=$(sed -n "s/^$secret_key=//p" "$single_config")
  [ "${#secret_value}" -eq 64 ]
  ! grep -F -q "$secret_value" "$single_log"
done
! grep -F -q "$master_key" "$single_log"
! grep -F -q "$backup_key" "$single_log"
grep -Eq '^FFDB_NODE_ID=[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' \
  "$single_config"
"$docker_path" compose \
  --env-file "$FFDB_INSTALL_ROOT/current/release.env" \
  --env-file "$single_config" \
  -f "$FFDB_INSTALL_ROOT/current/compose.single-host.yaml" config --quiet
services=$("$docker_path" compose \
  --env-file "$FFDB_INSTALL_ROOT/current/release.env" \
  --env-file "$single_config" \
  -f "$FFDB_INSTALL_ROOT/current/compose.single-host.yaml" config --services)
for service in postgres minio minio-bootstrap mailpit api sync-worker gateway; do
  printf '%s\n' "$services" | grep -qx "$service"
done
grep -q 'compose.single-host.yaml' "$FFDB_TEST_DOCKER_LOG"
"$FFDB_BIN_DIR/ffdb-host" status | grep -q 'active profile: single-host'
if "$FFDB_BIN_DIR/ffdb-host" start --profile external >/dev/null 2>&1; then
  printf '%s\n' "single-host installation unexpectedly allowed an in-place profile switch" >&2
  exit 1
fi
"$FFDB_BIN_DIR/ffdb-host" uninstall
[ -f "$single_config" ]
! grep -q -- '--volumes' "$FFDB_TEST_DOCKER_LOG"

github_site=$test_root/github-site
public_bin=$test_root/public-bin
github_releases_path=$github_repository/releases
github_releases=$github_site/$github_releases_path
install -d "$github_releases/latest/download" \
  "$github_releases/download/v0.1.0" "$github_releases/download/v0.2.0" \
  "$public_bin"
for release_dir in "$release_one" "$release_two"; do
  printf '{"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json"}\n' \
    > "$release_dir/SHA256SUMS.sigstore.json"
done
cp "$release_one"/* "$github_releases/download/v0.1.0/"
cp "$release_two"/* "$github_releases/download/v0.2.0/"
cp "$release_one/stable.txt" "$github_releases/latest/download/stable.txt"
cp "$release_one/install.sh" "$github_releases/latest/download/install.sh"
cp "$release_one/uninstall.sh" "$github_releases/latest/download/uninstall.sh"
cmp -s "$github_releases/latest/download/install.sh" \
  "$github_releases/download/v0.1.0/install.sh"

cat > "$public_bin/curl" <<'EOF'
#!/bin/sh
set -eu
headers=
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dump-header) headers=$2; shift 2 ;;
    --output|-o) output=$2; shift 2 ;;
    --header) printf '%s\n' "$2" >> "${FFDB_TEST_CURL_HEADERS:?}"; shift 2 ;;
    --proto|--max-redirs|--connect-timeout|--max-time) shift 2 ;;
    --tlsv1.2|--fail|--silent|--show-error|--location|-fsSL) shift ;;
    https://github.test/*) url=$1; shift ;;
    *) printf '%s\n' "unexpected curl argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$output" ] && [ -n "$url" ]
path=${url#https://github.test/}
source_file=${FFDB_TEST_GITHUB_SITE:?}/$path
[ -f "$source_file" ] || exit 22
[ -z "$headers" ] \
  || printf 'HTTP/2 200\r\ncontent-type: application/octet-stream\r\n\r\n' > "$headers"
cp "$source_file" "$output"
EOF
cat > "$public_bin/cosign" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "${FFDB_TEST_COSIGN_LOG:?}"
EOF
cat > "$public_bin/docker" <<'EOF'
#!/bin/sh
set -eu
if [ "${1:-}" = compose ]; then
  printf '%s\n' "$*" >> "${FFDB_TEST_DOCKER_LOG:?}"
  exit 0
fi
if [ "${1:-}" = buildx ] && [ "${2:-}" = version ]; then
  exit 0
fi
if [ "${1:-}" = buildx ] && [ "${2:-}" = imagetools ] && [ "${3:-}" = inspect ]; then
  printf '%s\n' "${4:?}" >> "${FFDB_TEST_IMAGE_LOG:?}"
  exit 0
fi
exit 2
EOF
chmod 0755 "$public_bin/curl" "$public_bin/cosign" "$public_bin/docker"
FFDB_TEST_COSIGN_LOG=$test_root/cosign.log
FFDB_TEST_IMAGE_LOG=$test_root/images.log
FFDB_TEST_CURL_HEADERS=$test_root/curl-headers.log
: > "$FFDB_TEST_CURL_HEADERS"
export FFDB_TEST_GITHUB_SITE=$github_site FFDB_TEST_COSIGN_LOG FFDB_TEST_IMAGE_LOG \
  FFDB_TEST_CURL_HEADERS

export FFDB_INSTALL_ROOT=$test_root/github-install/opt/ffdb
export FFDB_CONFIG_DIR=$test_root/github-install/etc/ffdb
export FFDB_CONFIG_FILE=$FFDB_CONFIG_DIR/ffdb.env
export FFDB_SINGLE_HOST_CONFIG_FILE=$FFDB_CONFIG_DIR/single-host.env
export FFDB_BIN_DIR=$test_root/github-install/bin
export FFDB_GITHUB_REPOSITORY=$github_repository
export FFDB_GITHUB_RELEASES_URL=$github_releases_url
export FFDB_GITHUB_TOKEN=fixture-token
export FFDB_REQUIRE_SIGNATURE=1
: > "$FFDB_TEST_DOCKER_LOG"
resolved_version=$(PATH=$public_bin:$PATH sh "$ROOT_DIR/infra/release/install.sh" --resolve-version)
[ "$resolved_version" = 0.1.0 ]
[ "$(PATH=$public_bin:$PATH sh "$ROOT_DIR/infra/release/install.sh" \
  --resolve-version --tag v0.1.0)" = 0.1.0 ]
if PATH=$public_bin:$PATH sh "$ROOT_DIR/infra/release/install.sh" \
  --resolve-version --version 0.1.0 --tag v0.2.0 >/dev/null 2>&1; then
  printf '%s\n' "installer accepted mismatched version and GitHub tag" >&2
  exit 1
fi
PATH=$public_bin:$PATH sh "$ROOT_DIR/infra/release/install.sh" \
  --profile single-host --start --require-signature
[ "$(readlink "$FFDB_INSTALL_ROOT/current")" = "$FFDB_INSTALL_ROOT/releases/0.1.0" ]
PATH=$public_bin:$PATH "$FFDB_BIN_DIR/ffdb-host" update-check \
  | grep -F -q 'FFDB 0.1.0 is up to date'
printf '0.2.0\n' > "$github_releases/latest/download/stable.txt"
PATH=$public_bin:$PATH "$FFDB_BIN_DIR/ffdb-host" update-check \
  | grep -F -q 'update available: FFDB 0.1.0 -> 0.2.0'
PATH=$public_bin:$PATH "$FFDB_BIN_DIR/ffdb-host" update
[ "$(readlink "$FFDB_INSTALL_ROOT/current")" = "$FFDB_INSTALL_ROOT/releases/0.2.0" ]
grep -F -q 'Authorization: Bearer fixture-token' "$FFDB_TEST_CURL_HEADERS"
PATH=$public_bin:$PATH "$FFDB_BIN_DIR/ffdb-host" uninstall

: > "$FFDB_TEST_COSIGN_LOG"
: > "$FFDB_TEST_IMAGE_LOG"
if ! FFDB_VERSION=0.1.0 PATH=$public_bin:$PATH \
  "$ROOT_DIR/scripts/check-public-distribution.sh" "$github_releases_url" \
  > "$test_root/public-check.log" \
  2> "$test_root/public-check.err"; then
  tail -n 200 "$test_root/public-check.err" >&2
  exit 1
fi
grep -F -q 'signed, reachable, and installable from GitHub Releases' "$test_root/public-check.log" || {
  sed -n '1,40p' "$test_root/public-check.log" >&2
  exit 1
}
grep -F -q 'verify-blob' "$FFDB_TEST_COSIGN_LOG" || {
  sed -n '1,20p' "$FFDB_TEST_COSIGN_LOG" >&2
  exit 1
}
cosign_image_checks=$(grep -c '^verify ' "$FFDB_TEST_COSIGN_LOG" || true)
[ "$cosign_image_checks" -eq 2 ] || {
  printf '%s\n' "expected two image signature checks, found $cosign_image_checks" >&2
  exit 1
}
image_reachability_checks=$(wc -l < "$FFDB_TEST_IMAGE_LOG" | tr -d ' ')
[ "$image_reachability_checks" -eq 5 ] || {
  printf '%s\n' "expected five image reachability checks, found $image_reachability_checks" >&2
  exit 1
}
mv "$github_releases/download/v0.1.0/SHA256SUMS.sigstore.json" \
  "$github_releases/download/v0.1.0/SHA256SUMS.sigstore.json.missing"
if FFDB_VERSION=0.1.0 PATH=$public_bin:$PATH \
  "$ROOT_DIR/scripts/check-public-distribution.sh" "$github_releases_url" \
  >/dev/null 2>&1; then
  printf '%s\n' "public distribution check accepted a missing signature bundle" >&2
  exit 1
fi

printf '%s\n' "release distribution tests passed"
