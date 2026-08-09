#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=${1:-${FFDB_VERSION:-}}
OUTPUT_DIR=${2:-${FFDB_SDK_OUTPUT_DIR:-$ROOT_DIR/dist/sdk}}

case "$OUTPUT_DIR" in
  /*) ;;
  *) OUTPUT_DIR=$ROOT_DIR/$OUTPUT_DIR ;;
esac

say() { printf '%s\n' "sdk packages: $*"; }
die() { printf '%s\n' "sdk packages: error: $*" >&2; exit 1; }

[ -n "$VERSION" ] || die "version is required"
printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
  || die "invalid version: $VERSION"
if [ -d "$OUTPUT_DIR" ] && [ -n "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  die "output directory must be empty: $OUTPUT_DIR"
fi
install -d -m 0755 "$OUTPUT_DIR"

cd "$ROOT_DIR"
node scripts/check-release-version.mjs "$VERSION"
node scripts/check-sdk-package-contract.mjs "$VERSION"
CI=true pnpm --filter @ffdb/client --filter @ffdb/sync-client \
  --filter @ffdb/react --filter @ffdb/react-native --filter @ffdb/email-components \
  --filter @ffdb/cli \
  --workspace-concurrency=1 build

package_dirs="client sync-client react react-native email-components cli"
for package_dir in $package_dirs; do
  manifest_version=$(node -p "require('./packages/$package_dir/package.json').version")
  [ "$manifest_version" = "$VERSION" ] \
    || die "packages/$package_dir is $manifest_version, expected $VERSION"
  CI=true pnpm --dir "packages/$package_dir" pack --pack-destination "$OUTPUT_DIR" >/dev/null
done

node scripts/check-sdk-package-contract.mjs "$VERSION" "$OUTPUT_DIR"

for package_dir in $package_dirs; do
  archive=ffdb-$package_dir-$VERSION.tgz
  [ -f "$OUTPUT_DIR/$archive" ] || die "missing archive: $archive"
  package_json=$(tar -xOf "$OUTPUT_DIR/$archive" package/package.json)
  printf '%s' "$package_json" | grep -F -q '"version": "'"$VERSION"'"' \
    || die "$archive contains the wrong version"
  if printf '%s' "$package_json" | grep -F -q 'workspace:'; then
    die "$archive contains an unresolved workspace dependency"
  fi
done

checksums=$OUTPUT_DIR/SDK-SHA256SUMS
: > "$checksums"
for package_dir in $package_dirs; do
  archive=ffdb-$package_dir-$VERSION.tgz
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$OUTPUT_DIR/$archive" | awk -v name="$archive" '{print $1 "  " name}' >> "$checksums"
  else
    shasum -a 256 "$OUTPUT_DIR/$archive" | awk -v name="$archive" '{print $1 "  " name}' >> "$checksums"
  fi
done

say "created six version-matched npm tarballs in $OUTPUT_DIR"
