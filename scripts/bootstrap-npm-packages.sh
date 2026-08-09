#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
RELEASE_VERSION=${1:-${FFDB_VERSION:-}}
BOOTSTRAP_VERSION=${FFDB_NPM_BOOTSTRAP_VERSION:-${RELEASE_VERSION:+$RELEASE_VERSION-bootstrap.0}}
NPM_ORGANIZATION=${FFDB_NPM_ORGANIZATION:-ffdb}
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-npm-bootstrap.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

say() { printf '%s\n' "npm bootstrap: $*"; }
die() { printf '%s\n' "npm bootstrap: error: $*" >&2; exit 1; }

[ -n "$RELEASE_VERSION" ] || die "release version is required"
printf '%s\n' "$RELEASE_VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
  || die "release version must be a stable semantic version: $RELEASE_VERSION"
printf '%s\n' "$BOOTSTRAP_VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+-[0-9A-Za-z][0-9A-Za-z.-]*$' \
  || die "invalid bootstrap version: $BOOTSTRAP_VERSION"
[ "$BOOTSTRAP_VERSION" != "$RELEASE_VERSION" ] \
  || die "bootstrap version must not consume the final release version"
[ "$NPM_ORGANIZATION" = ffdb ] \
  || die "package manifests use @ffdb; FFDB_NPM_ORGANIZATION must be ffdb"

command -v npm >/dev/null 2>&1 || die "npm is required"
command -v node >/dev/null 2>&1 || die "Node.js is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

npm_user=$(npm whoami 2>/dev/null) \
  || die "not authenticated to npm; run: npm login"
npm org ls "$NPM_ORGANIZATION" >/dev/null 2>&1 \
  || die "npm user $npm_user cannot access the $NPM_ORGANIZATION organization; create it or add this user before publishing"

cd "$ROOT_DIR"
scripts/build-sdk-packages.sh "$RELEASE_VERSION" "$work_dir/sdk"

package_dirs="client sync-client react react-native email-components cli"
for package_dir in $package_dirs; do
  package_name=@ffdb/$package_dir
  if npm view "$package_name@$BOOTSTRAP_VERSION" version >/dev/null 2>&1; then
    say "$package_name@$BOOTSTRAP_VERSION already exists; skipping"
    continue
  fi

  package_root=$work_dir/$package_dir/package
  install -d -m 0755 "$package_root"
  tar -xzf "$work_dir/sdk/ffdb-$package_dir-$RELEASE_VERSION.tgz" \
    -C "$work_dir/$package_dir"

  node - "$package_root/package.json" "$BOOTSTRAP_VERSION" <<'NODE'
const { readFileSync, writeFileSync } = require("node:fs");
const [manifestPath, version] = process.argv.slice(2);
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
manifest.version = version;
for (const name of Object.keys(manifest.dependencies ?? {})) {
  if (name.startsWith("@ffdb/")) manifest.dependencies[name] = version;
}
writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
NODE

  say "publishing $package_name@$BOOTSTRAP_VERSION with the non-default bootstrap tag"
  npm publish "$package_root" \
    --access public \
    --tag bootstrap \
    --provenance=false
done

say "all six bootstrap package identities exist; configure npm trusted publishing before creating the final release tag"
