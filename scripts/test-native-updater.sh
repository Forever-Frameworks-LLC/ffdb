#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-native-updater.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
release=$test_root/opt/ffdb/releases/0.3.2
jobs=$test_root/var/lib/ffdb/updater/jobs
requests=$test_root/var/lib/ffdb/update-requests
install -d "$release/systemd" "$jobs" "$requests" "$test_root/etc/ffdb"
printf '%s\n' 0.3.2 > "$release/VERSION"
cat > "$release/COMPATIBILITY" <<'EOF'
FFDB_NATIVE_STATE_SCHEMA=1
FFDB_NATIVE_MINIMUM_UPGRADE_VERSION=0.3.0
FFDB_NATIVE_MINIMUM_ROLLBACK_VERSION=0.3.0
EOF
ln -s "$release" "$test_root/opt/ffdb/current"
cat > "$release/systemd/ffdb-gateway.Caddyfile" <<'EOF'
https://ffdb.example.com { header X-Objects https://s3.example.com }
EOF
cat > "$test_root/etc/ffdb/ffdb.env" <<'EOF'
FFDB_PUBLIC_BASE_URL=https://ffdb.test
FFDB_S3_PUBLIC_ORIGIN=https://objects.ffdb.test
EOF
cat > "$test_root/var/lib/ffdb/updater/settings.json" <<'EOF'
{"channel":"stable","automatic_checks":true,"check_interval_hours":24,"automatic_apply":false,"maintenance_window_start":null,"maintenance_window_duration_minutes":60}
EOF
cat > "$test_root/var/lib/ffdb/updater/status.json" <<'EOF'
{"available_version":"0.3.3","last_check_at":"2026-08-10T12:00:00Z"}
EOF
cat > "$test_root/var/lib/ffdb/updater/available-release.json" <<'EOF'
{"version":"0.3.3","state_schema":1,"minimum_rollback_version":"0.3.0","signature_verified":true,"signature_identity":"https://github.com/Forever-Frameworks-LLC/ffdb/.github/workflows/release.yml@refs/tags/v0.3.3","release_url":"https://github.com/Forever-Frameworks-LLC/ffdb/releases/tag/v0.3.3"}
EOF

updater() {
  FFDB_UPDATER_TEST_MODE=1 FFDB_UPDATER_TEST_ROOT="$test_root" \
    "$ROOT_DIR/infra/release/native/ffdb-update" "$@"
}

updater inspect > "$test_root/inspect.json"
jq -e '
  .installed_version == "0.3.2"
  and .available_version == "0.3.3"
  and .update_available
  and .state_schema == 1
  and .minimum_rollback_version == "0.3.0"
  and .capabilities == {check:true,install:true,rollback:true,automatic_checks:true,automatic_apply:true}
  and .settings.channel == "stable"
  and .releases[0].state_schema == 1
  and .releases[1].version == "0.3.3"
  and .releases[1].signature_verified == true
  and .releases[1].release_url == "https://github.com/Forever-Frameworks-LLC/ffdb/releases/tag/v0.3.3"
' "$test_root/inspect.json" >/dev/null

updater submit check > "$test_root/check-job.json"
check_id=$(jq -r .job_id "$test_root/check-job.json")
updater job "$check_id" | jq -e '.operation == "check" and .state == "queued"' >/dev/null
if updater job 00000000-0000-4000-8000-000000000000 \
  > /dev/null 2> "$test_root/not-found.json"; then
  printf '%s\n' "native updater returned a missing job" >&2
  exit 1
fi
jq -e '.code == "not_found" and .retryable == false' "$test_root/not-found.json" >/dev/null

updater submit install 0.3.3 > "$test_root/install-job.json"
jq -e '.operation == "install" and .requested_version == "0.3.3"' \
  "$test_root/install-job.json" >/dev/null

updater submit rollback 0.3.0 > "$test_root/rollback-job.json"
jq -e '.operation == "rollback" and .requested_version == "0.3.0"' \
  "$test_root/rollback-job.json" >/dev/null

updater submit configure --channel stable --automatic-checks true \
  --check-interval-hours 12 --automatic-apply false \
  --maintenance-window disabled > "$test_root/configure-job.json"
jq -e '.operation == "configure" and .state == "queued"' \
  "$test_root/configure-job.json" >/dev/null

# The activation helper must restore the prior release atomically when the new
# release never becomes ready. The second readiness call represents the prior
# release recovering successfully.
next_release=$test_root/opt/ffdb/releases/0.3.3
install -d "$next_release/systemd"
printf '%s\n' 0.3.3 > "$next_release/VERSION"
cp "$release/COMPATIBILITY" "$next_release/COMPATIBILITY"
cat > "$next_release/systemd/ffdb-gateway.Caddyfile" <<'EOF'
https://ffdb.example.com { header X-Release next header X-Objects https://s3.example.com }
EOF
FFDB_UPDATER_TEST_MODE=1
FFDB_UPDATER_TEST_ROOT=$test_root
FFDB_UPDATER_LIBRARY_MODE=1
export FFDB_UPDATER_TEST_MODE FFDB_UPDATER_TEST_ROOT FFDB_UPDATER_LIBRARY_MODE
. "$ROOT_DIR/infra/release/native/ffdb-update"
readiness_attempt=0
restart_and_wait() {
  readiness_attempt=$((readiness_attempt + 1))
  [ "$readiness_attempt" -gt 1 ]
}
if activate_with_health_or_restore "$next_release" "$release"; then
  printf '%s\n' "failed release unexpectedly passed readiness" >&2
  exit 1
fi
[ "$(readlink "$test_root/opt/ffdb/current")" = "$release" ]
grep -F -q 'https://ffdb.test' "$test_root/etc/ffdb/Caddyfile"
! grep -F -q 'X-Release next' "$test_root/etc/ffdb/Caddyfile"
unset FFDB_UPDATER_LIBRARY_MODE

if updater submit install latest >/dev/null 2>&1; then
  printf '%s\n' "native updater accepted a non-version install target" >&2
  exit 1
fi
if updater submit configure --channel edge --automatic-checks true \
  --check-interval-hours 12 --automatic-apply false \
  --maintenance-window disabled >/dev/null 2>&1; then
  printf '%s\n' "native updater accepted an unsupported release channel" >&2
  exit 1
fi
if updater submit configure --channel stable --automatic-checks true \
  --check-interval-hours 0 --automatic-apply false \
  --maintenance-window disabled >/dev/null 2>&1; then
  printf '%s\n' "native updater accepted an unsafe check interval" >&2
  exit 1
fi
if updater submit configure --channel stable --automatic-checks true \
  --check-interval-hours 12 --automatic-apply true \
  --maintenance-window disabled >/dev/null 2>&1; then
  printf '%s\n' "native updater accepted automatic apply without a maintenance window" >&2
  exit 1
fi

queued=$(find "$requests" -type f -name '*.json' | wc -l | tr -d ' ')
while [ "$queued" -lt 32 ]; do
  updater submit check >/dev/null
  queued=$((queued + 1))
done
if updater submit check > /dev/null 2> "$test_root/busy.json"; then
  printf '%s\n' "native updater accepted more than 32 queued requests" >&2
  exit 1
fi
jq -e '.code == "busy" and .retryable == true' "$test_root/busy.json" >/dev/null
[ "$(find "$requests" -type f -name '*.json' | wc -l | tr -d ' ')" = 32 ]
printf '%s\n' "native updater test: constrained protocol and inspect state passed"
