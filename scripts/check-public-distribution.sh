#!/bin/sh
set -eu

GITHUB_REPOSITORY=${FFDB_GITHUB_REPOSITORY:-Forever-Frameworks-LLC/ffdb}
RELEASES_URL=${1:-${FFDB_GITHUB_RELEASES_URL:-${FFDB_DISTRIBUTION_BASE_URL:-https://github.com/$GITHUB_REPOSITORY/releases}}}
RELEASES_URL=${RELEASES_URL%/}
REQUIRE_FULL_VERIFICATION=${FFDB_DISTRIBUTION_REQUIRE_FULL_VERIFICATION:-1}
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-public-distribution.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

die() { printf '%s\n' "distribution check: error: $*" >&2; exit 1; }
warn() { printf '%s\n' "distribution check: warning: $*" >&2; }
case "$REQUIRE_FULL_VERIFICATION" in
  0|1) ;;
  *) die "FFDB_DISTRIBUTION_REQUIRE_FULL_VERIFICATION must be 0 or 1" ;;
esac

fetch() {
  url=$1
  output=$2
  headers=$output.headers
  set -- --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    --max-redirs 5 --connect-timeout 10 --max-time 60 \
    --dump-header "$headers" --output "$output"
  if [ -n "${FFDB_GITHUB_TOKEN:-}" ]; then
    set -- "$@" --header "Authorization: Bearer $FFDB_GITHUB_TOKEN"
  fi
  curl "$@" "$url" || die "$url is not available to a non-browser client"
  content_type=$(awk 'BEGIN {IGNORECASE=1} /^content-type:/ {value=$0} END {sub(/^[^:]*:[[:space:]]*/, "", value); sub(/\r$/, "", value); print value}' "$headers")
  case "$content_type" in
    text/html*) die "$url returned HTML (an interactive challenge or routing fallback)" ;;
  esac
  if grep -Eiq '<!doctype[[:space:]]+html|<html([[:space:]>])' "$output"; then
    die "$url returned an HTML challenge body"
  fi
}

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    command -v shasum >/dev/null 2>&1 || die "sha256sum or shasum is required"
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

verify_checksum() {
  asset=$1
  file=$2
  expected=$(awk -v name="$asset" '$2 == name {print $1; exit}' "$work_dir/SHA256SUMS")
  [ -n "$expected" ] || die "$asset is absent from SHA256SUMS"
  actual=$(file_sha256 "$file")
  [ "$actual" = "$expected" ] || die "$asset checksum does not match"
}

limited_or_die() {
  message=$1
  if [ "$REQUIRE_FULL_VERIFICATION" = 1 ]; then
    die "$message"
  fi
  warn "$message"
  limited_verification=1
}

printf '%s\n' "$GITHUB_REPOSITORY" \
  | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' \
  || die "GitHub repository must be OWNER/REPO: $GITHUB_REPOSITORY"

version=${FFDB_VERSION:-}
if [ -z "$version" ]; then
  fetch "$RELEASES_URL/latest/download/stable.txt" "$work_dir/stable-resolved.txt"
  version=$(tr -d '\r\n' < "$work_dir/stable-resolved.txt")
  version=${version#v}
fi
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
  || die "GitHub Releases did not resolve a valid FFDB version"
release_tag=v$version
release=$RELEASES_URL/download/$release_tag

fetch "$release/install.sh" "$work_dir/install.sh"
fetch "$release/uninstall.sh" "$work_dir/uninstall.sh"
fetch "$release/stable.txt" "$work_dir/stable.txt"
sh -n "$work_dir/install.sh" "$work_dir/uninstall.sh" \
  || die "published shell entry points are not syntactically valid"
grep -F -q 'FFDB_GITHUB_RELEASES_URL' "$work_dir/install.sh" \
  || die "install.sh is not the FFDB distribution installer"
[ "$(tr -d '\r\n' < "$work_dir/stable.txt")" = "$version" ] \
  || die "release stable.txt does not match $version"

fetch "$release/SHA256SUMS" "$work_dir/SHA256SUMS"
fetch "$release/SHA256SUMS.sigstore.json" "$work_dir/SHA256SUMS.sigstore.json"
[ -s "$work_dir/SHA256SUMS.sigstore.json" ] || die "SHA256SUMS.sigstore.json is empty"

verify_checksum install.sh "$work_dir/install.sh"
verify_checksum uninstall.sh "$work_dir/uninstall.sh"
verify_checksum stable.txt "$work_dir/stable.txt"

for asset in "ffdb-compose-bundle-$version.tar.gz" "ffdb-host-$version" \
  release-manifest.json SDK-SHA256SUMS; do
  fetch "$release/$asset" "$work_dir/$asset"
  verify_checksum "$asset" "$work_dir/$asset"
done

command -v node >/dev/null 2>&1 \
  || die "Node.js is required to validate release-manifest.json"
expected_identity=${FFDB_SIGNATURE_IDENTITY:-https://github.com/$GITHUB_REPOSITORY/.github/workflows/release.yml@refs/tags/$release_tag}
expected_issuer=https://token.actions.githubusercontent.com
node - "$work_dir/release-manifest.json" "$version" "$release_tag" "$GITHUB_REPOSITORY" \
  "$release" "$expected_identity" "$expected_issuer" \
  > "$work_dir/manifest-values" <<'NODE'
const [manifestPath, expectedVersion, expectedTag, expectedRepository,
  expectedReleaseUrl, expectedIdentity, expectedIssuer] = process.argv.slice(2);
const { readFileSync } = require("node:fs");
let manifest;
try {
  manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
} catch {
  throw new Error("release-manifest.json is not valid JSON");
}
const exactArray = (value, expected) => Array.isArray(value)
  && value.length === expected.length
  && expected.every((entry) => value.includes(entry));
if (manifest.schema_version !== 2
    || manifest.version !== expectedVersion
    || manifest.release_tag !== expectedTag
    || manifest.github_repository !== expectedRepository
    || manifest.github_release_url !== expectedReleaseUrl
    || !exactArray(manifest.architectures, ["linux/amd64", "linux/arm64"])
    || !exactArray(manifest.profiles, ["external", "single-host"])
    || manifest.signature_identity !== expectedIdentity
    || manifest.signature_issuer !== expectedIssuer
    || manifest.native_update?.state_schema !== 1
    || manifest.native_update?.minimum_upgrade_version !== "0.3.0"
    || manifest.native_update?.minimum_rollback_version !== "0.3.0"
    || manifest.native_update?.assets?.amd64 !== `ffdb-native-linux-amd64-${expectedVersion}.tar.gz`
    || manifest.native_update?.assets?.arm64 !== `ffdb-native-linux-arm64-${expectedVersion}.tar.gz`) {
  throw new Error("release-manifest.json does not match the canonical release contract");
}
const images = [
  manifest.runtime_image,
  manifest.gateway_image,
  manifest.single_host_images?.postgres,
  manifest.single_host_images?.minio,
  manifest.single_host_images?.mailpit,
];
const imagePattern = /^[A-Za-z0-9][A-Za-z0-9._/:+-]*@sha256:[0-9a-f]{64}$/;
if (images.some((image) => typeof image !== "string" || !imagePattern.test(image))) {
  throw new Error("release-manifest.json contains an unpinned or invalid image reference");
}
process.stdout.write(images.join("\n") + "\n");
NODE

runtime_image=$(sed -n '1p' "$work_dir/manifest-values")
gateway_image=$(sed -n '2p' "$work_dir/manifest-values")
postgres_image=$(sed -n '3p' "$work_dir/manifest-values")
minio_image=$(sed -n '4p' "$work_dir/manifest-values")
mailpit_image=$(sed -n '5p' "$work_dir/manifest-values")

for arch in amd64 arm64; do
  asset=ffdb-native-linux-$arch-$version.tar.gz
  fetch "$release/$asset" "$work_dir/$asset"
  verify_checksum "$asset" "$work_dir/$asset"
  archive_root=ffdb-native-$version
  tar -tzf "$work_dir/$asset" > "$work_dir/$asset.list" \
    || die "$asset is not a valid gzip-compressed tar archive"
  for required_path in install-native.sh uninstall-native.sh VERSION COMPATIBILITY \
    bin/ffdb-api bin/ffdb-database-worker bin/ffdb-sync-worker bin/ffdb-update \
    web/index.html web/docs/index.html web/app/index.html \
    systemd/ffdb-gateway.Caddyfile systemd/ffdb-gateway.service \
    systemd/ffdb-update-agent.path systemd/ffdb-update-agent.service \
    systemd/ffdb-update-check.service systemd/ffdb-update-check.timer; do
    grep -F -x -q "$archive_root/$required_path" "$work_dir/$asset.list" \
      || die "$asset is missing $archive_root/$required_path"
  done
  archived_version=$(tar -xOf "$work_dir/$asset" "$archive_root/VERSION" | tr -d '\r\n')
  [ "$archived_version" = "$version" ] || die "$asset contains the wrong version"
  tar -xOf "$work_dir/$asset" "$archive_root/COMPATIBILITY" \
    | grep -F -q 'FFDB_NATIVE_STATE_SCHEMA=1' \
    || die "$asset contains an unsupported native state schema"
  tar -xOf "$work_dir/$asset" "$archive_root/COMPATIBILITY" \
    | grep -F -q 'FFDB_NATIVE_MINIMUM_ROLLBACK_VERSION=0.3.0' \
    || die "$asset contains an unsupported rollback floor"
done

for package in client sync-client react react-native email-components cli; do
  asset=ffdb-$package-$version.tgz
  case "$package" in
    client) package_name=@ffdb/client ;;
    cli) package_name=@ffdb/cli ;;
    *) package_name=@ffdb/$package ;;
  esac
  fetch "$release/$asset" "$work_dir/$asset"
  release_expected=$(awk -v name="$asset" '$2 == name {print $1; exit}' "$work_dir/SHA256SUMS")
  sdk_expected=$(awk -v name="$asset" '$2 == name {print $1; exit}' "$work_dir/SDK-SHA256SUMS")
  [ -n "$release_expected" ] || die "$asset is absent from SHA256SUMS"
  [ -n "$sdk_expected" ] || die "$asset is absent from SDK-SHA256SUMS"
  [ "$release_expected" = "$sdk_expected" ] \
    || die "$asset has conflicting release and SDK checksums"
  actual=$(file_sha256 "$work_dir/$asset")
  [ "$actual" = "$sdk_expected" ] || die "$asset checksum does not match"
  package_json=$(tar -xOf "$work_dir/$asset" package/package.json 2>/dev/null) \
    || die "$asset is not a valid npm package archive"
  printf '%s' "$package_json" | node -e '
    let source = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { source += chunk; });
    process.stdin.on("end", () => {
      const manifest = JSON.parse(source);
      if (manifest.version !== process.argv[1]) process.exitCode = 2;
      if (JSON.stringify(manifest).includes("workspace:")) process.exitCode = 3;
      if (manifest.name !== process.argv[2]) process.exitCode = 4;
    });
  ' "$version" "$package_name" \
    || die "$asset contains the wrong name/version or an unresolved workspace dependency"
done

limited_verification=0
if command -v cosign >/dev/null 2>&1; then
  cosign verify-blob "$work_dir/SHA256SUMS" \
    --bundle "$work_dir/SHA256SUMS.sigstore.json" \
    --certificate-identity "$expected_identity" \
    --certificate-oidc-issuer "$expected_issuer" >/dev/null \
    || die "SHA256SUMS signature verification failed"
  for image in "$runtime_image" "$gateway_image"; do
    cosign verify "$image" \
      --certificate-identity "$expected_identity" \
      --certificate-oidc-issuer "$expected_issuer" >/dev/null \
      || die "FFDB image signature verification failed: $image"
  done
else
  limited_or_die "cosign is required to verify SHA256SUMS and FFDB image signatures"
fi

if command -v docker >/dev/null 2>&1 \
  && docker buildx version >/dev/null 2>&1; then
  for image in "$runtime_image" "$gateway_image" "$postgres_image" "$minio_image" "$mailpit_image"; do
    docker buildx imagetools inspect "$image" >/dev/null \
      || die "pinned image is not reachable: $image"
  done
elif command -v crane >/dev/null 2>&1; then
  for image in "$runtime_image" "$gateway_image" "$postgres_image" "$minio_image" "$mailpit_image"; do
    crane manifest "$image" >/dev/null \
      || die "pinned image is not reachable: $image"
  done
else
  limited_or_die "Docker Buildx or crane is required to verify all pinned image digests are reachable"
fi

if [ "$limited_verification" = 1 ]; then
  printf '%s\n' "distribution check: FFDB $version GitHub release assets are coherent at $RELEASES_URL; full installability was not verified"
else
  printf '%s\n' "distribution check: FFDB $version is signed, reachable, and installable from GitHub Releases at $RELEASES_URL"
fi
