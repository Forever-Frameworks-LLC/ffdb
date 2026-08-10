#!/bin/sh
set -eu

test_root=$(mktemp -d /tmp/ffdb-native-install.XXXXXX)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
bundle=$test_root/bundle
fake_bin=$test_root/fake-bin
command_log=$test_root/commands.log
install -d "$bundle/bin" "$bundle/web/docs" "$bundle/web/app" "$bundle/systemd" "$fake_bin"

cp /src/infra/release/native/install-native.sh "$bundle/install-native.sh"
cp /src/infra/release/native/ffdb-update "$bundle/bin/ffdb-update"
cp /src/infra/systemd/* "$bundle/systemd/"
chmod 0755 "$bundle/install-native.sh"
chmod 0755 "$bundle/bin/ffdb-update"
printf '%s\n' '#!/bin/sh' 'exit 0' > "$bundle/ffdb-backup"
chmod 0755 "$bundle/ffdb-backup"
for binary in ffdb-api ffdb-database-worker ffdb-sync-worker; do
  printf '%s\n' '#!/bin/sh' 'exit 0' > "$bundle/bin/$binary"
  chmod 0755 "$bundle/bin/$binary"
done
printf '%s\n' '<!doctype html><title>landing</title>' > "$bundle/web/index.html"
printf '%s\n' '<!doctype html><title>docs</title>' > "$bundle/web/docs/index.html"
printf '%s\n' '<!doctype html><title>portal</title>' > "$bundle/web/app/index.html"
printf '%s\n' '0.3.0' > "$bundle/VERSION"
cat > "$bundle/COMPATIBILITY" <<'EOF'
FFDB_NATIVE_STATE_SCHEMA=1
FFDB_NATIVE_MINIMUM_UPGRADE_VERSION=0.3.0
FFDB_NATIVE_MINIMUM_ROLLBACK_VERSION=0.3.0
EOF

for command in pg_dump pg_restore sqlite3 curl tar jq cosign flock sha256sum diff; do
  printf '%s\n' '#!/bin/sh' 'exit 0' > "$fake_bin/$command"
  chmod 0755 "$fake_bin/$command"
done
cat > "$fake_bin/systemctl" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "${FFDB_TEST_COMMAND_LOG:?}"
EOF
cat > "$fake_bin/systemd-sysusers" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$fake_bin/systemd-tmpfiles" <<'EOF'
#!/bin/sh
set -eu
[ "$(stat -c '%U:%G:%a' /var/lib/ffdb)" = root:ffdb:750 ]
EOF
cat > "$fake_bin/caddy" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = validate ] || exit 2
config=
while [ "$#" -gt 0 ]; do
  [ "$1" = --config ] && { config=$2; break; }
  shift
done
[ -f "$config" ]
grep -F -q 'https://prod.ffdb.test' "$config"
grep -F -q 'https://objects.ffdb.test' "$config"
EOF
chmod 0755 "$fake_bin/systemctl" "$fake_bin/systemd-sysusers" \
  "$fake_bin/systemd-tmpfiles" "$fake_bin/caddy"

# The container is disposable, so deterministic test identities are safe.
grep -q '^ffdb:' /etc/group || printf '%s\n' 'ffdb:x:12345:' >> /etc/group
grep -q '^ffdb:' /etc/passwd \
  || printf '%s\n' 'ffdb:x:12345:12345:FFDB:/var/lib/ffdb:/usr/sbin/nologin' >> /etc/passwd
install -d -m 0750 -o root -g ffdb /etc/ffdb
install -d -m 0750 -o ffdb -g ffdb /var/lib/ffdb
for path in projects backups metrics sync caddy; do
  install -d -m 0700 -o ffdb -g ffdb "/var/lib/ffdb/$path"
done
install -d -m 0755 /var/www/ffdb

cat > "$test_root/ffdb.env" <<'EOF'
FFDB_PUBLIC_BASE_URL=https://prod.ffdb.test
FFDB_S3_PUBLIC_ORIGIN=https://objects.ffdb.test
FFDB_TRUSTED_PROXY_CIDRS=127.0.0.1/32,::1/128
EOF

FFDB_TEST_COMMAND_LOG=$command_log \
PATH="$fake_bin:$PATH" \
  "$bundle/install-native.sh" --env-file "$test_root/ffdb.env"

test ! -f /opt/ffdb/releases/0.3.0/.signature-verified

FFDB_TEST_COMMAND_LOG=$command_log \
PATH="$fake_bin:$PATH" \
  "$bundle/install-native.sh" --verified-release --start

test -x /usr/local/bin/ffdb-api
test -x /usr/local/bin/ffdb-database-worker
test -x /usr/local/bin/ffdb-sync-worker
test -x /usr/local/bin/ffdb-update
test "$(readlink /opt/ffdb/current)" = /opt/ffdb/releases/0.3.0
test -f /opt/ffdb/releases/0.3.0/COMPATIBILITY
test -f /opt/ffdb/releases/0.3.0/.signature-verified
grep -F -q 'refs/tags/v0.3.0' /opt/ffdb/releases/0.3.0/.signature-identity
grep -F -q '/releases/tag/v0.3.0' /opt/ffdb/releases/0.3.0/.release-url
test -f /etc/systemd/system/ffdb-gateway.service
test -f /etc/systemd/system/ffdb-update-agent.path
test -f /etc/systemd/system/ffdb-update-check.timer
test -f /etc/ffdb/Caddyfile
test -f /var/www/ffdb/index.html
test "$(stat -c '%a' /etc/ffdb/ffdb.env)" = 640
test "$(stat -c '%a' /etc/ffdb/Caddyfile)" = 640
test "$(stat -c '%U:%G:%a' /var/lib/ffdb)" = root:ffdb:750
test "$(stat -c '%U:%G:%a' /var/lib/ffdb/projects)" = ffdb:ffdb:700
! grep -F -q 'example.com' /etc/ffdb/Caddyfile
grep -F -q 'handle @metrics {' /etc/ffdb/Caddyfile
grep -F -q 'respond 404' /etc/ffdb/Caddyfile
grep -F -q 'enable --now ffdb-update-agent.path ffdb-update-check.timer' "$command_log"
grep -F -q 'restart ffdb-api.service ffdb-sync-worker.service ffdb-gateway.service' "$command_log"
