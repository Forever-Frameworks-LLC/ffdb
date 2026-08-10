#!/bin/sh
set -eu

purge=0
yes=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --purge-data) purge=1; shift ;;
    --yes) yes=1; shift ;;
    *) printf '%s\n' "unknown option: $1" >&2; exit 1 ;;
  esac
done
[ "$(id -u)" -eq 0 ] || { printf '%s\n' "run as root" >&2; exit 1; }
if [ "$purge" -eq 1 ] && [ "$yes" -ne 1 ]; then
  printf '%s\n' "--purge-data also requires --yes" >&2
  exit 1
fi

systemctl disable --now ffdb-update-check.timer ffdb-update-agent.path \
  ffdb-gateway.service ffdb-sync-worker.service ffdb-api.service 2>/dev/null || true
rm -f /etc/systemd/system/ffdb-api.service /etc/systemd/system/ffdb-sync-worker.service \
  /etc/systemd/system/ffdb-gateway.service \
  /etc/systemd/system/ffdb-update-agent.path \
  /etc/systemd/system/ffdb-update-agent.service \
  /etc/systemd/system/ffdb-update-check.service \
  /etc/systemd/system/ffdb-update-check.timer
rm -f /etc/systemd/system/ffdb-update-agent.service.d/ffdb-extraction-compat.conf
rmdir /etc/systemd/system/ffdb-update-agent.service.d 2>/dev/null || true
rm -f /usr/local/bin/ffdb-api /usr/local/bin/ffdb-database-worker \
  /usr/local/bin/ffdb-sync-worker /usr/local/bin/ffdb-backup /usr/local/bin/ffdb-update
rm -f /etc/ffdb/Caddyfile
rm -rf /var/www/ffdb
rm -rf /opt/ffdb/releases /opt/ffdb/current /var/cache/ffdb-updater
systemctl daemon-reload
if [ "$purge" -eq 1 ]; then
  rm -rf /var/lib/ffdb /etc/ffdb
  printf '%s\n' "removed native FFDB software, configuration, and data"
else
  printf '%s\n' "removed native FFDB software; preserved /var/lib/ffdb and /etc/ffdb"
fi
