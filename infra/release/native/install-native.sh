#!/bin/sh
set -eu

bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
config_source=
replace_config=0
start_after=0

die() { printf '%s\n' "ffdb native installer: error: $*" >&2; exit 1; }
say() { printf '%s\n' "ffdb native installer: $*"; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --env-file) [ "$#" -ge 2 ] || die "--env-file requires a value"; config_source=$2; shift 2 ;;
    --replace-config) replace_config=1; shift ;;
    --start) start_after=1; shift ;;
    -h|--help)
      printf '%s\n' "Usage: install-native.sh [--env-file PATH] [--replace-config] [--start]"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[ "$(uname -s)" = Linux ] || die "native bundles support Linux only"
[ "$(id -u)" -eq 0 ] || die "run the native installer as root"
command -v systemctl >/dev/null 2>&1 || die "systemd is required"
command -v caddy >/dev/null 2>&1 || die "Caddy is required"
for required_command in pg_dump pg_restore sqlite3 curl tar; do
  command -v "$required_command" >/dev/null 2>&1 \
    || die "$required_command is required for the installed backup workflow"
done

install -D -m 0644 "$bundle_dir/systemd/ffdb.sysusers.conf" /etc/sysusers.d/ffdb.conf
systemd-sysusers /etc/sysusers.d/ffdb.conf
install -D -m 0644 "$bundle_dir/systemd/ffdb.tmpfiles.conf" /etc/tmpfiles.d/ffdb.conf
systemd-tmpfiles --create /etc/tmpfiles.d/ffdb.conf
install -m 0755 "$bundle_dir/bin/ffdb-api" /usr/local/bin/ffdb-api
install -m 0755 "$bundle_dir/bin/ffdb-database-worker" /usr/local/bin/ffdb-database-worker
install -m 0755 "$bundle_dir/bin/ffdb-sync-worker" /usr/local/bin/ffdb-sync-worker
install -m 0755 "$bundle_dir/ffdb-backup" /usr/local/bin/ffdb-backup
install -m 0644 "$bundle_dir/systemd/ffdb-api.service" /etc/systemd/system/ffdb-api.service
install -m 0644 "$bundle_dir/systemd/ffdb-sync-worker.service" /etc/systemd/system/ffdb-sync-worker.service
install -m 0644 "$bundle_dir/systemd/ffdb-gateway.service" /etc/systemd/system/ffdb-gateway.service

if [ -n "$config_source" ]; then
  [ -f "$config_source" ] || die "configuration file not found: $config_source"
  if [ -e /etc/ffdb/ffdb.env ] && [ "$replace_config" -ne 1 ]; then
    say "preserving existing /etc/ffdb/ffdb.env"
  else
    install -m 0640 -o root -g ffdb "$config_source" /etc/ffdb/ffdb.env
  fi
elif [ ! -e /etc/ffdb/ffdb.env ]; then
  install -m 0640 -o root -g ffdb "$bundle_dir/systemd/ffdb.env.example" /etc/ffdb/ffdb.env
fi

web_staging=/var/www/.ffdb-web.$$
rm -rf "$web_staging"
install -d -m 0755 "$web_staging"
cp -R "$bundle_dir/web/." "$web_staging/"
rm -rf /var/www/ffdb
mv "$web_staging" /var/www/ffdb
chown -R root:root /var/www/ffdb
find /var/www/ffdb -type d -exec chmod 0755 {} \;
find /var/www/ffdb -type f -exec chmod 0644 {} \;

s3_origin=$(awk -F= '$1 == "FFDB_S3_PUBLIC_ORIGIN" {sub(/^[^=]*=/, ""); print; exit}' /etc/ffdb/ffdb.env)
public_origin=$(awk -F= '$1 == "FFDB_PUBLIC_BASE_URL" {sub(/^[^=]*=/, ""); print; exit}' /etc/ffdb/ffdb.env)
printf '%s\n' "$s3_origin" | grep -Eq '^https://[A-Za-z0-9.-]+(:[0-9]+)?$' \
  || die "FFDB_S3_PUBLIC_ORIGIN must be an exact HTTPS origin before installing Caddy"
printf '%s\n' "$public_origin" | grep -Eq '^https://[A-Za-z0-9.-]+(:[0-9]+)?$' \
  || die "FFDB_PUBLIC_BASE_URL must be an exact HTTPS origin before installing Caddy"
sed -e "s|https://s3.example.com|$s3_origin|g" \
  -e "s|https://ffdb.example.com|$public_origin|g" \
  "$bundle_dir/systemd/ffdb-gateway.Caddyfile" > /etc/ffdb/Caddyfile
chown root:ffdb /etc/ffdb/Caddyfile
chmod 0640 /etc/ffdb/Caddyfile
printf '%s\n' "$(sed -n '1p' "$bundle_dir/VERSION")" > /var/lib/ffdb/installed-version

systemctl daemon-reload
caddy validate --config /etc/ffdb/Caddyfile --adapter caddyfile
if [ "$start_after" -eq 1 ]; then
  if grep -Eq 'replace-me|example\.com' /etc/ffdb/ffdb.env; then
    die "/etc/ffdb/ffdb.env still contains placeholders"
  fi
  systemctl enable --now ffdb-api.service ffdb-sync-worker.service
  systemctl enable --now ffdb-gateway.service
  say "native FFDB services started"
else
  say "native artifacts installed; review /etc/ffdb/ffdb.env, then enable the services"
fi
