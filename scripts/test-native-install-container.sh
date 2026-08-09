#!/bin/sh
set -eu

test_root=$(mktemp -d /tmp/ffdb-native-install.XXXXXX)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
bundle=$test_root/bundle
fake_bin=$test_root/fake-bin
command_log=$test_root/commands.log
install -d "$bundle/bin" "$bundle/web/docs" "$bundle/web/app" "$bundle/systemd" "$fake_bin"

cp /src/infra/release/native/install-native.sh "$bundle/install-native.sh"
cp /src/infra/systemd/* "$bundle/systemd/"
chmod 0755 "$bundle/install-native.sh"
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

for command in pg_dump pg_restore sqlite3 curl tar; do
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
exit 0
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
  "$bundle/install-native.sh" --env-file "$test_root/ffdb.env" --start

test -x /usr/local/bin/ffdb-api
test -x /usr/local/bin/ffdb-database-worker
test -x /usr/local/bin/ffdb-sync-worker
test -f /etc/systemd/system/ffdb-gateway.service
test -f /etc/ffdb/Caddyfile
test -f /var/www/ffdb/index.html
test "$(stat -c '%a' /etc/ffdb/ffdb.env)" = 640
test "$(stat -c '%a' /etc/ffdb/Caddyfile)" = 640
! grep -F -q 'example.com' /etc/ffdb/Caddyfile
grep -F -q 'enable --now ffdb-api.service ffdb-sync-worker.service' "$command_log"
grep -F -q 'enable --now ffdb-gateway.service' "$command_log"
