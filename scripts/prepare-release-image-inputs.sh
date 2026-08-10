#!/bin/sh
set -eu

VERSION=${1:-}
INPUT_DIR=${2:-}
OUTPUT_DIR=${3:-}

die() { printf '%s\n' "release image inputs: error: $*" >&2; exit 1; }
printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
  || die "invalid version"
[ -d "$INPUT_DIR" ] || die "missing input directory: $INPUT_DIR"
[ -n "$OUTPUT_DIR" ] || die "missing output directory"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-release-images.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

for arch in amd64 arm64; do
  archive=$INPUT_DIR/ffdb-native-linux-$arch-$VERSION.tar.gz
  [ -f "$archive" ] || die "missing native archive: $archive"

  extracted=$work_dir/$arch
  install -d "$extracted"
  tar -xzf "$archive" -C "$extracted"
  root=$extracted/ffdb-native-$VERSION
  [ -f "$root/VERSION" ] || die "$archive does not contain VERSION"
  [ "$(sed -n '1p' "$root/VERSION")" = "$VERSION" ] \
    || die "$archive contains a different version"

  install -d "$OUTPUT_DIR/runtime/$arch"
  for binary in ffdb-api ffdb-database-worker ffdb-sync-worker; do
    [ -x "$root/bin/$binary" ] || die "$archive is missing executable $binary"
    install -m 0755 "$root/bin/$binary" "$OUTPUT_DIR/runtime/$arch/$binary"
  done

  if [ "$arch" = amd64 ]; then
    for path in index.html docs/index.html app/index.html; do
      [ -f "$root/web/$path" ] || die "$archive is missing web asset $path"
    done
    install -d "$OUTPUT_DIR/web"
    cp -R "$root/web/." "$OUTPUT_DIR/web/"
  fi
done

printf '%s\n' "release image inputs: prepared $OUTPUT_DIR"
