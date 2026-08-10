#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=${1:-${FFDB_VERSION:-}}
RUNTIME_IMAGE=${2:-${FFDB_RUNTIME_IMAGE:-}}
GATEWAY_IMAGE=${3:-${FFDB_GATEWAY_IMAGE:-}}
OUTPUT_DIR=${4:-${FFDB_RELEASE_OUTPUT_DIR:-$ROOT_DIR/dist/release}}
POSTGRES_IMAGE=${FFDB_POSTGRES_IMAGE:-}
MINIO_IMAGE=${FFDB_MINIO_IMAGE:-}
MAILPIT_IMAGE=${FFDB_MAILPIT_IMAGE:-}
GITHUB_REPOSITORY=${FFDB_GITHUB_REPOSITORY:-Forever-Frameworks-LLC/ffdb}
GITHUB_RELEASES_URL=${FFDB_GITHUB_RELEASES_URL:-}

say() { printf '%s\n' "release bundle: $*"; }
die() { printf '%s\n' "release bundle: error: $*" >&2; exit 1; }

[ -n "$VERSION" ] && [ -n "$RUNTIME_IMAGE" ] && [ -n "$GATEWAY_IMAGE" ] && \
  [ -n "$POSTGRES_IMAGE" ] && [ -n "$MINIO_IMAGE" ] && [ -n "$MAILPIT_IMAGE" ] \
  || die "runtime, gateway, PostgreSQL, MinIO, and Mailpit digest-pinned images are required"
printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$' \
  || die "invalid version: $VERSION"
printf '%s\n' "$GITHUB_REPOSITORY" \
  | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' \
  || die "GitHub repository must be OWNER/REPO: $GITHUB_REPOSITORY"
[ -n "$GITHUB_RELEASES_URL" ] \
  || GITHUB_RELEASES_URL=https://github.com/$GITHUB_REPOSITORY/releases
GITHUB_RELEASES_URL=${GITHUB_RELEASES_URL%/}
printf '%s\n' "$GITHUB_RELEASES_URL" \
  | grep -Eq "^https://[^[:space:]\"']+$" \
  || die "GitHub Releases URL must be an HTTPS URL without whitespace or quotes"
RELEASE_TAG=v$VERSION
for image in "$RUNTIME_IMAGE" "$GATEWAY_IMAGE" "$POSTGRES_IMAGE" "$MINIO_IMAGE" "$MAILPIT_IMAGE"; do
  printf '%s\n' "$image" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9._/:+-]*@sha256:[0-9a-f]{64}$' \
    || die "image is not pinned by a sha256 digest: $image"
done

if [ -d "$OUTPUT_DIR" ] && [ -n "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  die "output directory must be empty: $OUTPUT_DIR"
fi
install -d -m 0755 "$OUTPUT_DIR"
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-release.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
bundle_root=$work_dir/ffdb-$VERSION
install -d -m 0755 "$bundle_root"

SIGNATURE_IDENTITY=${FFDB_SIGNATURE_IDENTITY:-https://github.com/$GITHUB_REPOSITORY/.github/workflows/release.yml@refs/tags/$RELEASE_TAG}
sed \
  -e "s|@VERSION@|$VERSION|g" \
  -e "s|@RUNTIME_IMAGE@|$RUNTIME_IMAGE|g" \
  -e "s|@GATEWAY_IMAGE@|$GATEWAY_IMAGE|g" \
  -e "s|@POSTGRES_IMAGE@|$POSTGRES_IMAGE|g" \
  -e "s|@MINIO_IMAGE@|$MINIO_IMAGE|g" \
  -e "s|@MAILPIT_IMAGE@|$MAILPIT_IMAGE|g" \
  -e "s|@SIGNATURE_IDENTITY@|$SIGNATURE_IDENTITY|g" \
  "$ROOT_DIR/infra/release/release.env.in" > "$bundle_root/release.env"
sed -e "s|@VERSION@|$VERSION|g" \
  "$ROOT_DIR/infra/release/ffdb-host.in" > "$bundle_root/ffdb-host"
chmod 0755 "$bundle_root/ffdb-host"
sed -e "s|@VERSION@|$VERSION|g" \
  "$ROOT_DIR/infra/release/ffdb-backup.in" > "$bundle_root/ffdb-backup"
chmod 0755 "$bundle_root/ffdb-backup"
install -m 0644 "$ROOT_DIR/infra/release/compose.yaml" "$bundle_root/compose.yaml"
install -m 0644 "$ROOT_DIR/infra/release/compose.single-host.yaml" \
  "$bundle_root/compose.single-host.yaml"
install -m 0600 "$ROOT_DIR/infra/release/ffdb.env.example" "$bundle_root/ffdb.env.example"
install -m 0755 "$ROOT_DIR/infra/release/install.sh" "$bundle_root/install.sh"
install -m 0755 "$ROOT_DIR/infra/release/uninstall.sh" "$bundle_root/uninstall.sh"
printf '%s\n' "$VERSION" > "$bundle_root/VERSION"

bundle_name=ffdb-compose-bundle-$VERSION.tar.gz
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
  epoch=${SOURCE_DATE_EPOCH:-0}
  tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner \
    -czf "$OUTPUT_DIR/$bundle_name" -C "$work_dir" "ffdb-$VERSION"
else
  warn_message="non-GNU tar detected; archive is valid but deterministic timestamps require GNU tar"
  printf '%s\n' "release bundle: warning: $warn_message" >&2
  tar -czf "$OUTPUT_DIR/$bundle_name" -C "$work_dir" "ffdb-$VERSION"
fi

host_name=ffdb-host-$VERSION
install -m 0755 "$bundle_root/ffdb-host" "$OUTPUT_DIR/$host_name"
install -m 0755 "$ROOT_DIR/infra/release/install.sh" "$OUTPUT_DIR/install.sh"
install -m 0755 "$ROOT_DIR/infra/release/uninstall.sh" "$OUTPUT_DIR/uninstall.sh"
cat > "$OUTPUT_DIR/release-manifest.json" <<EOF
{
  "schema_version": 2,
  "version": "$VERSION",
  "release_tag": "$RELEASE_TAG",
  "github_repository": "$GITHUB_REPOSITORY",
  "github_release_url": "$GITHUB_RELEASES_URL/download/$RELEASE_TAG",
  "architectures": ["linux/amd64", "linux/arm64"],
  "profiles": ["external", "single-host"],
  "runtime_image": "$RUNTIME_IMAGE",
  "gateway_image": "$GATEWAY_IMAGE",
  "single_host_images": {
    "postgres": "$POSTGRES_IMAGE",
    "minio": "$MINIO_IMAGE",
    "mailpit": "$MAILPIT_IMAGE"
  },
  "native_update": {
    "state_schema": 1,
    "minimum_upgrade_version": "0.3.0",
    "minimum_rollback_version": "0.3.0",
    "assets": {
      "amd64": "ffdb-native-linux-amd64-$VERSION.tar.gz",
      "arm64": "ffdb-native-linux-arm64-$VERSION.tar.gz"
    }
  },
  "signature_identity": "$SIGNATURE_IDENTITY",
  "signature_issuer": "https://token.actions.githubusercontent.com"
}
EOF
printf '%s\n' "$VERSION" > "$OUTPUT_DIR/stable.txt"

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

host_sha=$(file_sha256 "$OUTPUT_DIR/$host_name")
asset_url=$GITHUB_RELEASES_URL/download/$RELEASE_TAG/$host_name
sed \
  -e "s|@VERSION@|$VERSION|g" \
  -e "s|@ASSET_URL@|$asset_url|g" \
  -e "s|@SHA256@|$host_sha|g" \
  "$ROOT_DIR/infra/release/homebrew/ffdb-host.rb.in" > "$OUTPUT_DIR/ffdb-host.rb"

if [ -n "${FFDB_EXTRA_ASSETS_DIR:-}" ]; then
  [ -d "$FFDB_EXTRA_ASSETS_DIR" ] || die "FFDB_EXTRA_ASSETS_DIR is not a directory"
  cp "$FFDB_EXTRA_ASSETS_DIR"/* "$OUTPUT_DIR/"
fi

checksums=$OUTPUT_DIR/SHA256SUMS
: > "$checksums"
find "$OUTPUT_DIR" -maxdepth 1 -type f ! -name SHA256SUMS -print | sort |
  while IFS= read -r asset_path; do
    asset=$(basename "$asset_path")
    printf '%s  %s\n' "$(file_sha256 "$asset_path")" "$asset" >> "$checksums"
  done

say "created $OUTPUT_DIR/$bundle_name"
say "runtime: $RUNTIME_IMAGE"
say "gateway: $GATEWAY_IMAGE"
say "single-host infrastructure images are digest-pinned in release metadata"
