#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-start}"
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$APP_DIR"

show_usage() {
  cat <<'USAGE'
usage: ./script/build_and_run.sh [mode]

Modes:
  start, run                 Start the Expo dev server
  --ios, ios                 Start Expo and open iOS
  --android, android         Start Expo and open Android
  --web, web                 Start Expo for web
  --dev-client, dev-client   Start with a custom development client
  --tunnel, tunnel           Start through an Expo tunnel
  --export-web, export-web   Export the web bundle
  --doctor, doctor           Validate Expo package compatibility
  --help, help               Show this help
USAGE
}

if command -v pnpm >/dev/null 2>&1; then
  EXPO_CMD=(pnpm --config.verify-deps-before-run=false exec expo)
else
  EXPO_CMD=(npx expo)
fi

case "$MODE" in
  start|run)
    exec "${EXPO_CMD[@]}" start
    ;;
  --ios|ios)
    exec "${EXPO_CMD[@]}" start --ios
    ;;
  --android|android)
    exec "${EXPO_CMD[@]}" start --android
    ;;
  --web|web)
    exec "${EXPO_CMD[@]}" start --web
    ;;
  --dev-client|dev-client)
    exec "${EXPO_CMD[@]}" start --dev-client
    ;;
  --tunnel|tunnel)
    exec "${EXPO_CMD[@]}" start --tunnel
    ;;
  --export-web|export-web)
    exec "${EXPO_CMD[@]}" export --platform web
    ;;
  --doctor|doctor)
    exec "${EXPO_CMD[@]}" install --check
    ;;
  --help|help)
    show_usage
    ;;
  *)
    show_usage >&2
    exit 2
    ;;
esac
