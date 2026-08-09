#!/bin/sh
set -eu

host_command=${FFDB_HOST_COMMAND:-/usr/local/bin/ffdb-host}
if [ ! -x "$host_command" ]; then
  printf '%s\n' "ffdb uninstaller: ffdb-host is not installed at $host_command" >&2
  exit 1
fi

exec "$host_command" uninstall "$@"
