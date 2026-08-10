#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-image-resolution-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
fake_docker=$test_root/docker
digest=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa

cat > "$fake_docker" <<'EOF'
#!/bin/sh
set -eu

[ "$#" -eq 4 ]
[ "$1" = buildx ]
[ "$2" = imagetools ]
[ "$3" = inspect ]

case "${FFDB_FAKE_MODE:-valid}" in
  valid)
    cat <<MANIFEST
Name:      docker.io/library/example:tag
Digest:    sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  Platform:    linux/amd64
  Platform:    linux/arm64/v8
MANIFEST
    ;;
  missing-arm64)
    cat <<MANIFEST
Name:      docker.io/library/example:tag
Digest:    sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  Platform:    linux/amd64
MANIFEST
    ;;
  invalid-digest)
    cat <<MANIFEST
Name:      docker.io/library/example:tag
Digest:    sha256:not-a-digest
  Platform:    linux/amd64
  Platform:    linux/arm64/v8
MANIFEST
    ;;
  docker-failure)
    exit 9
    ;;
esac
EOF
chmod 0755 "$fake_docker"

resolved=$(FFDB_DOCKER_BIN="$fake_docker" FFDB_FAKE_MODE=valid \
  "$ROOT_DIR/scripts/resolve-release-image.sh" example:tag)
[ "$resolved" = "example:tag@sha256:$digest" ]

for mode in missing-arm64 invalid-digest docker-failure; do
  if FFDB_DOCKER_BIN="$fake_docker" FFDB_FAKE_MODE=$mode \
    "$ROOT_DIR/scripts/resolve-release-image.sh" example:tag \
    >"$test_root/$mode.out" 2>"$test_root/$mode.err"; then
    printf '%s\n' "release image resolver unexpectedly accepted $mode" >&2
    exit 1
  fi
done
grep -F -q 'does not publish linux/arm64' "$test_root/missing-arm64.err"
grep -F -q 'could not resolve manifest digest' "$test_root/invalid-digest.err"

printf '%s\n' "release image resolution tests passed"
