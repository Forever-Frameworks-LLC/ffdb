#!/bin/sh
set -eu

bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
config_source=
replace_config=0
start_after=0
verified_release=0
install_root=/opt/ffdb
releases_root=$install_root/releases
current_link=$install_root/current

die() { printf '%s\n' "ffdb native installer: error: $*" >&2; exit 1; }
say() { printf '%s\n' "ffdb native installer: $*"; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --env-file) [ "$#" -ge 2 ] || die "--env-file requires a value"; config_source=$2; shift 2 ;;
    --replace-config) replace_config=1; shift ;;
    --start) start_after=1; shift ;;
    --verified-release) verified_release=1; shift ;;
    -h|--help)
      printf '%s\n' "Usage: install-native.sh [--env-file PATH] [--replace-config] [--start] [--verified-release]"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[ "$(uname -s)" = Linux ] || die "native bundles support Linux only"
[ "$(id -u)" -eq 0 ] || die "run the native installer as root"
command -v systemctl >/dev/null 2>&1 || die "systemd is required"
command -v caddy >/dev/null 2>&1 || die "Caddy is required"
for required_command in pg_dump pg_restore sqlite3 curl tar jq cosign flock sha256sum diff; do
  command -v "$required_command" >/dev/null 2>&1 \
    || die "$required_command is required for the installed backup workflow"
done

install -D -m 0644 "$bundle_dir/systemd/ffdb.sysusers.conf" /etc/sysusers.d/ffdb.conf
systemd-sysusers /etc/sysusers.d/ffdb.conf
install -D -m 0644 "$bundle_dir/systemd/ffdb.tmpfiles.conf" /etc/tmpfiles.d/ffdb.conf
systemd-tmpfiles --create /etc/tmpfiles.d/ffdb.conf
version=$(sed -n '1p' "$bundle_dir/VERSION")
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
  || die "bundle VERSION is invalid"
[ -f "$bundle_dir/COMPATIBILITY" ] || die "bundle is missing COMPATIBILITY"
state_schema=$(awk -F= '$1 == "FFDB_NATIVE_STATE_SCHEMA" {print $2; exit}' "$bundle_dir/COMPATIBILITY")
minimum_upgrade=$(awk -F= '$1 == "FFDB_NATIVE_MINIMUM_UPGRADE_VERSION" {print $2; exit}' "$bundle_dir/COMPATIBILITY")
minimum_rollback=$(awk -F= '$1 == "FFDB_NATIVE_MINIMUM_ROLLBACK_VERSION" {print $2; exit}' "$bundle_dir/COMPATIBILITY")
printf '%s\n' "$state_schema" | grep -Eq '^[0-9]+$' || die "native state schema is invalid"
for compatibility_version in "$minimum_upgrade" "$minimum_rollback"; do
  printf '%s\n' "$compatibility_version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
    || die "native compatibility version is invalid"
done

install -d -m 0755 "$install_root" "$releases_root"
release_dir=$releases_root/$version
if [ ! -d "$release_dir" ]; then
  release_staging=$(mktemp -d "$install_root/.native-install.XXXXXX")
  trap 'rm -rf "$release_staging"' EXIT HUP INT TERM
  install -d -m 0755 "$release_staging/release"
  cp -R "$bundle_dir/." "$release_staging/release/"
  chown -R root:root "$release_staging/release"
  find "$release_staging/release" -type d -exec chmod 0755 {} \;
  mv "$release_staging/release" "$release_dir"
  rmdir "$release_staging"
  trap - EXIT HUP INT TERM
else
  [ "$(sed -n '1p' "$release_dir/VERSION")" = "$version" ] \
    || die "existing release directory has inconsistent VERSION"
fi

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

activate_tmp=$install_root/.current.$$
rm -f "$activate_tmp"
ln -s "$release_dir" "$activate_tmp"
mv -Tf "$activate_tmp" "$current_link"

for unit in ffdb-api.service ffdb-sync-worker.service ffdb-gateway.service \
  ffdb-update-agent.path ffdb-update-agent.service \
  ffdb-update-check.service ffdb-update-check.timer; do
  unit_tmp=/etc/systemd/system/.$unit.$$
  rm -f "$unit_tmp"
  ln -s "$current_link/systemd/$unit" "$unit_tmp"
  mv -Tf "$unit_tmp" "/etc/systemd/system/$unit"
done

for binary in ffdb-api ffdb-database-worker ffdb-sync-worker; do
  link_tmp=/usr/local/bin/.$binary.$$
  rm -f "$link_tmp"
  ln -s "$current_link/bin/$binary" "$link_tmp"
  mv -Tf "$link_tmp" "/usr/local/bin/$binary"
done
for installed_tool in ffdb-backup ffdb-update; do
  link_tmp=/usr/local/bin/.$installed_tool.$$
  rm -f "$link_tmp"
  if [ "$installed_tool" = ffdb-backup ]; then
    link_target=$current_link/ffdb-backup
  else
    link_target=$current_link/bin/ffdb-update
  fi
  ln -s "$link_target" "$link_tmp"
  mv -Tf "$link_tmp" "/usr/local/bin/$installed_tool"
done

web_tmp=/var/www/.ffdb.$$
rm -f "$web_tmp"
ln -s "$current_link/web" "$web_tmp"
if [ -d /var/www/ffdb ] && [ ! -L /var/www/ffdb ]; then rm -rf /var/www/ffdb; fi
mv -Tf "$web_tmp" /var/www/ffdb
printf '%s\n' "$version" > /var/lib/ffdb/installed-version
if [ "$verified_release" -eq 1 ]; then
  install -m 0644 /dev/null "$release_dir/.signature-verified"
  printf '%s\n' "https://github.com/Forever-Frameworks-LLC/ffdb/.github/workflows/release.yml@refs/tags/v$version" \
    > "$release_dir/.signature-identity"
  printf '%s\n' "https://github.com/Forever-Frameworks-LLC/ffdb/releases/tag/v$version" \
    > "$release_dir/.release-url"
fi

systemctl daemon-reload
caddy validate --config /etc/ffdb/Caddyfile --adapter caddyfile
/usr/local/bin/ffdb-update initialize
systemctl enable --now ffdb-update-agent.path ffdb-update-check.timer
if [ "$start_after" -eq 1 ]; then
  if grep -Eq 'replace-me|example\.com' /etc/ffdb/ffdb.env; then
    die "/etc/ffdb/ffdb.env still contains placeholders"
  fi
  systemctl enable ffdb-api.service ffdb-sync-worker.service ffdb-gateway.service
  systemctl restart ffdb-api.service ffdb-sync-worker.service ffdb-gateway.service
  say "native FFDB services started"
else
  say "native artifacts installed; review /etc/ffdb/ffdb.env, then enable the services"
fi
