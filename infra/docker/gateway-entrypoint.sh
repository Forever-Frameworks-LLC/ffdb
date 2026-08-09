#!/bin/sh
set -eu

# /tmp is a runtime tmpfs in the supported Compose configurations. Creating the
# output directory here keeps the image compatible with a read-only rootfs while
# allowing nginx's official envsubst helper to run as the unprivileged nginx user.
mkdir -p "$NGINX_ENVSUBST_OUTPUT_DIR"
chmod 0700 "$NGINX_ENVSUBST_OUTPUT_DIR"

exec /docker-entrypoint.sh "$@"
