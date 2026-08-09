#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
docker_config=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-docker-config.XXXXXX")
trap 'rm -rf "$docker_config"' EXIT HUP INT TERM
printf '%s\n' '{"auths":{}}' > "$docker_config/config.json"
command -v docker >/dev/null 2>&1 || {
  printf '%s\n' "native Linux test: docker is required" >&2
  exit 1
}

# Parse the exact Caddyfile shipped in the native bundle with the real gateway
# binary, then exercise the installer against an isolated Debian filesystem.
DOCKER_CONFIG=$docker_config docker run --rm \
  -v "$ROOT_DIR:/src:ro" \
  caddy:2.10.2-alpine \
  caddy validate --config /src/infra/systemd/ffdb-gateway.Caddyfile --adapter caddyfile

DOCKER_CONFIG=$docker_config docker run --rm \
  -v "$ROOT_DIR:/src:ro" \
  debian:bookworm-slim \
  /src/scripts/test-native-install-container.sh

printf '%s\n' "native Linux test: Caddy configuration and installer passed"
