#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=${1:-}
ARCH=${2:-}
BINARY_DIR=${3:-}
WEB_DIR=${4:-}
OUTPUT_DIR=${5:-}
STATE_SCHEMA=${FFDB_NATIVE_STATE_SCHEMA:-1}
MINIMUM_UPGRADE_VERSION=${FFDB_NATIVE_MINIMUM_UPGRADE_VERSION:-0.3.0}
MINIMUM_ROLLBACK_VERSION=${FFDB_NATIVE_MINIMUM_ROLLBACK_VERSION:-0.3.0}

die() { printf '%s\n' "native bundle: error: $*" >&2; exit 1; }
printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
  || die "invalid version"
case "$ARCH" in amd64|arm64) ;; *) die "architecture must be amd64 or arm64" ;; esac
printf '%s\n' "$STATE_SCHEMA" | grep -Eq '^[0-9]+$' || die "native state schema must be an integer"
for compatibility_version in "$MINIMUM_UPGRADE_VERSION" "$MINIMUM_ROLLBACK_VERSION"; do
  printf '%s\n' "$compatibility_version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
    || die "invalid native compatibility version"
done
[ -d "$BINARY_DIR" ] && [ -d "$WEB_DIR" ] && [ -n "$OUTPUT_DIR" ] || die "missing input directory"
for binary in ffdb-api ffdb-database-worker ffdb-sync-worker; do
  [ -x "$BINARY_DIR/$binary" ] || die "missing executable $BINARY_DIR/$binary"
done
for path in index.html docs/index.html app/index.html; do
  [ -f "$WEB_DIR/$path" ] || die "missing web asset $WEB_DIR/$path"
done

install -d "$OUTPUT_DIR"
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-native.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
root=$work_dir/ffdb-native-$VERSION
install -d "$root/bin" "$root/web" "$root/systemd"
for binary in ffdb-api ffdb-database-worker ffdb-sync-worker; do
  install -m 0755 "$BINARY_DIR/$binary" "$root/bin/$binary"
done
install -m 0755 "$ROOT_DIR/infra/release/native/ffdb-update" "$root/bin/ffdb-update"
cp -R "$WEB_DIR/." "$root/web/"
cp "$ROOT_DIR"/infra/systemd/* "$root/systemd/"
install -m 0755 "$ROOT_DIR/infra/release/native/install-native.sh" "$root/install-native.sh"
install -m 0755 "$ROOT_DIR/infra/release/native/uninstall-native.sh" "$root/uninstall-native.sh"
sed -e "s|@VERSION@|$VERSION|g" \
  "$ROOT_DIR/infra/release/ffdb-backup.in" > "$root/ffdb-backup"
chmod 0755 "$root/ffdb-backup"
printf '%s\n' "$VERSION" > "$root/VERSION"
cat > "$root/COMPATIBILITY" <<EOF
FFDB_NATIVE_STATE_SCHEMA=$STATE_SCHEMA
FFDB_NATIVE_MINIMUM_UPGRADE_VERSION=$MINIMUM_UPGRADE_VERSION
FFDB_NATIVE_MINIMUM_ROLLBACK_VERSION=$MINIMUM_ROLLBACK_VERSION
EOF

archive=$OUTPUT_DIR/ffdb-native-linux-$ARCH-$VERSION.tar.gz
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
  tar --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
    -czf "$archive" -C "$work_dir" "ffdb-native-$VERSION"
else
  tar -czf "$archive" -C "$work_dir" "ffdb-native-$VERSION"
fi
printf '%s\n' "native bundle: created $archive"
