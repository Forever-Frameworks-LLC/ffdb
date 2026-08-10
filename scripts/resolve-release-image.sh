#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  printf '%s\n' "usage: resolve-release-image.sh <image:tag>" >&2
  exit 2
fi

source_image=$1
docker_bin=${FFDB_DOCKER_BIN:-docker}

command -v "$docker_bin" >/dev/null 2>&1 || {
  printf '%s\n' "release image resolver: $docker_bin is required" >&2
  exit 1
}

inspection=$($docker_bin buildx imagetools inspect "$source_image")
for platform in linux/amd64 linux/arm64; do
  printf '%s\n' "$inspection" \
    | grep -Eq "Platform:[[:space:]]+$platform(/[^[:space:]]+)?([[:space:]]|$)" || {
      printf '%s\n' "$source_image does not publish $platform" >&2
      exit 1
    }
done

digest=$(printf '%s\n' "$inspection" | awk '$1 == "Digest:" {print $2; exit}')
case "$digest" in
  sha256:*) digest_hex=${digest#sha256:} ;;
  *)
    printf '%s\n' "could not resolve manifest digest for $source_image" >&2
    exit 1
    ;;
esac
if [ "${#digest_hex}" -ne 64 ]; then
  printf '%s\n' "could not resolve manifest digest for $source_image" >&2
  exit 1
fi
case "$digest_hex" in
  *[!0-9a-f]*)
    printf '%s\n' "could not resolve manifest digest for $source_image" >&2
    exit 1
    ;;
esac

printf '%s@%s\n' "$source_image" "$digest"
