#!/bin/sh
set -eu

DEFAULT_GITHUB_REPOSITORY=Forever-Frameworks-LLC/ffdb
GITHUB_REPOSITORY=${FFDB_GITHUB_REPOSITORY:-$DEFAULT_GITHUB_REPOSITORY}
GITHUB_RELEASES_URL=${FFDB_GITHUB_RELEASES_URL:-}
VERSION=${FFDB_VERSION:-}
RELEASE_TAG=${FFDB_RELEASE_TAG:-}
RELEASE_BASE=${FFDB_RELEASE_BASE_URL:-}
STABLE_URL=${FFDB_STABLE_URL:-}
START_AFTER=0
REPLACE_CONFIG=0
REQUIRE_SIGNATURE=${FFDB_REQUIRE_SIGNATURE:-0}
RESOLVE_ONLY=0
CONFIG_SOURCE=
PROFILE=${FFDB_INSTALL_PROFILE:-}

say() { printf '%s\n' "ffdb installer: $*"; }
warn() { printf '%s\n' "ffdb installer: warning: $*" >&2; }
die() { printf '%s\n' "ffdb installer: error: $*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: install.sh [options]

Options:
  --version VERSION       Install an exact version (or set FFDB_VERSION)
  --tag TAG               Install an exact vVERSION GitHub release tag
  --repository OWNER/REPO Select a GitHub repository
  --github-releases URL   Override the GitHub Releases root URL
  --release-base URL      Exact directory containing this version's assets
  --stable-url URL        Override latest stable-version resolution
  --profile PROFILE       external (default) or single-host evaluation
  --env-file PATH         Install an existing production environment file
  --replace-config        Replace an existing /etc/ffdb/ffdb.env
  --start                 Pull images and start after installation
  --require-signature     Fail unless cosign verifies release and image signatures
  --resolve-version       Print the selected version without installing

Testing/mirror overrides:
  FFDB_GITHUB_REPOSITORY=OWNER/REPO
  FFDB_GITHUB_RELEASES_URL=https://github.example/OWNER/REPO/releases
  FFDB_RELEASE_BASE_URL=file:///absolute/release-directory
  FFDB_STABLE_URL=file:///absolute/stable.txt
  FFDB_INSTALL_ROOT, FFDB_CONFIG_DIR, FFDB_CONFIG_FILE, FFDB_BIN_DIR
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) [ "$#" -ge 2 ] || die "--version requires a value"; VERSION=$2; shift 2 ;;
    --tag) [ "$#" -ge 2 ] || die "--tag requires a value"; RELEASE_TAG=$2; shift 2 ;;
    --repository) [ "$#" -ge 2 ] || die "--repository requires a value"; GITHUB_REPOSITORY=$2; shift 2 ;;
    --github-releases) [ "$#" -ge 2 ] || die "--github-releases requires a value"; GITHUB_RELEASES_URL=$2; shift 2 ;;
    --release-base) [ "$#" -ge 2 ] || die "--release-base requires a value"; RELEASE_BASE=$2; shift 2 ;;
    --stable-url) [ "$#" -ge 2 ] || die "--stable-url requires a value"; STABLE_URL=$2; shift 2 ;;
    --profile) [ "$#" -ge 2 ] || die "--profile requires a value"; PROFILE=$2; shift 2 ;;
    --env-file) [ "$#" -ge 2 ] || die "--env-file requires a value"; CONFIG_SOURCE=$2; shift 2 ;;
    --replace-config) REPLACE_CONFIG=1; shift ;;
    --start) START_AFTER=1; shift ;;
    --require-signature) REQUIRE_SIGNATURE=1; shift ;;
    --resolve-version) RESOLVE_ONLY=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

command -v curl >/dev/null 2>&1 || die "curl is required"
printf '%s\n' "$GITHUB_REPOSITORY" \
  | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' \
  || die "GitHub repository must be OWNER/REPO: $GITHUB_REPOSITORY"
[ -n "$GITHUB_RELEASES_URL" ] \
  || GITHUB_RELEASES_URL=https://github.com/$GITHUB_REPOSITORY/releases
GITHUB_RELEASES_URL=${GITHUB_RELEASES_URL%/}
[ -n "$STABLE_URL" ] || STABLE_URL=$GITHUB_RELEASES_URL/latest/download/stable.txt

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-install.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

download() {
  url=$1
  output=$2
  if [ -n "${FFDB_GITHUB_TOKEN:-}" ]; then
    curl -fsSL --header "Authorization: Bearer $FFDB_GITHUB_TOKEN" "$url" -o "$output"
  else
    curl -fsSL "$url" -o "$output"
  fi
}

if [ -z "$VERSION" ]; then
  if [ -n "$RELEASE_TAG" ]; then
    VERSION=${RELEASE_TAG#v}
  else
    download "$STABLE_URL" "$tmp_dir/stable-resolved.txt" \
      || die "could not resolve the latest stable FFDB release from $STABLE_URL"
    VERSION=$(tr -d '\r\n' < "$tmp_dir/stable-resolved.txt")
    VERSION=${VERSION#v}
  fi
fi
printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
  || die "invalid version: $VERSION"
[ -n "$RELEASE_TAG" ] || RELEASE_TAG=v$VERSION
[ "$RELEASE_TAG" = "v$VERSION" ] \
  || die "release tag $RELEASE_TAG does not match version $VERSION"

if [ "$RESOLVE_ONLY" -eq 1 ]; then
  printf '%s\n' "$VERSION"
  exit 0
fi

[ "$(id -u)" -eq 0 ] || [ "${FFDB_ALLOW_UNPRIVILEGED:-0}" = 1 ] \
  || die "run the installer as root"
command -v tar >/dev/null 2>&1 || die "tar is required"
command -v cmp >/dev/null 2>&1 || die "cmp is required"

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64|Linux:aarch64|Linux:arm64|Darwin:x86_64|Darwin:arm64) ;;
  *) die "supported hosts are Linux/macOS on amd64 or arm64 with Docker Compose" ;;
esac

if [ -z "$RELEASE_BASE" ]; then
  RELEASE_BASE=$GITHUB_RELEASES_URL/download/$RELEASE_TAG
fi
RELEASE_BASE=${RELEASE_BASE%/}
bundle_name=ffdb-compose-bundle-$VERSION.tar.gz
host_name=ffdb-host-$VERSION

fetch() {
  asset=$1
  download "$RELEASE_BASE/$asset" "$tmp_dir/$asset"
}

say "downloading FFDB $VERSION release metadata"
fetch SHA256SUMS || die "could not download SHA256SUMS"
if fetch SHA256SUMS.sigstore.json 2>/dev/null; then
  if command -v cosign >/dev/null 2>&1; then
    identity=${FFDB_SIGNATURE_IDENTITY:-https://github.com/$GITHUB_REPOSITORY/.github/workflows/release.yml@refs/tags/$RELEASE_TAG}
    cosign verify-blob "$tmp_dir/SHA256SUMS" \
      --bundle "$tmp_dir/SHA256SUMS.sigstore.json" \
      --certificate-identity "$identity" \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com >/dev/null \
      || die "release signature verification failed"
    say "verified release signature"
  elif [ "$REQUIRE_SIGNATURE" = 1 ]; then
    die "cosign is required by --require-signature"
  else
    warn "cosign is unavailable; continuing with mandatory SHA-256 verification"
  fi
elif [ "$REQUIRE_SIGNATURE" = 1 ]; then
  die "release signature bundle is unavailable"
else
  warn "release signature bundle is unavailable; continuing with SHA-256 verification"
fi

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

verify_asset() {
  asset=$1
  expected=$(awk -v wanted="$asset" '$2 == wanted {print $1; exit}' "$tmp_dir/SHA256SUMS")
  [ -n "$expected" ] || die "$asset is missing from SHA256SUMS"
  actual=$(file_sha256 "$tmp_dir/$asset")
  [ "$actual" = "$expected" ] || die "SHA-256 mismatch for $asset"
}

for asset in stable.txt release-manifest.json install.sh uninstall.sh \
  "$bundle_name" "$host_name"; do
  fetch "$asset" || die "could not download $asset from GitHub release $RELEASE_TAG"
  verify_asset "$asset"
done
[ "$(tr -d '\r\n' < "$tmp_dir/stable.txt")" = "$VERSION" ] \
  || die "release stable.txt does not match $VERSION"
grep -F -q '"schema_version": 2' "$tmp_dir/release-manifest.json" \
  || die "release manifest schema is unsupported"
grep -F -q "\"version\": \"$VERSION\"" "$tmp_dir/release-manifest.json" \
  || die "release manifest version does not match $VERSION"
grep -F -q "\"release_tag\": \"$RELEASE_TAG\"" "$tmp_dir/release-manifest.json" \
  || die "release manifest tag does not match $RELEASE_TAG"
grep -F -q "\"github_repository\": \"$GITHUB_REPOSITORY\"" \
  "$tmp_dir/release-manifest.json" \
  || die "release manifest repository does not match $GITHUB_REPOSITORY"
archive_root=ffdb-$VERSION
tar -xOf "$tmp_dir/$bundle_name" "$archive_root/install.sh" \
  | cmp -s - "$tmp_dir/install.sh" \
  || die "release bundle install.sh differs from the checksummed standalone asset"
tar -xOf "$tmp_dir/$bundle_name" "$archive_root/uninstall.sh" \
  | cmp -s - "$tmp_dir/uninstall.sh" \
  || die "release bundle uninstall.sh differs from the checksummed standalone asset"
tar -xOf "$tmp_dir/$bundle_name" "$archive_root/ffdb-host" \
  | cmp -s - "$tmp_dir/$host_name" \
  || die "release bundle ffdb-host differs from the checksummed standalone asset"
chmod 0755 "$tmp_dir/$host_name"
say "verified GitHub release checksums and startup scripts"

set -- install --version "$VERSION" --bundle "$tmp_dir/$bundle_name"
[ -z "$PROFILE" ] || set -- "$@" --profile "$PROFILE"
if [ -n "$CONFIG_SOURCE" ]; then
  [ -f "$CONFIG_SOURCE" ] || die "configuration file not found: $CONFIG_SOURCE"
  set -- "$@" --env-file "$CONFIG_SOURCE"
fi
[ "$REPLACE_CONFIG" -eq 0 ] || set -- "$@" --replace-config
[ "$START_AFTER" -eq 0 ] || set -- "$@" --start
[ "$REQUIRE_SIGNATURE" = 0 ] || set -- "$@" --require-signature

"$tmp_dir/$host_name" "$@"
