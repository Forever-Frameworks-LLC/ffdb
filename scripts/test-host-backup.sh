#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-host-backup-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
fake_bin=$test_root/fake-bin
state_root=$test_root/state
config_file=$test_root/ffdb.env
version_file=$test_root/installed-version
service_state=$test_root/services
command_log=$test_root/commands.log
secret=database-password-must-not-be-logged
install -d "$fake_bin" "$state_root/projects" "$state_root/metrics" \
  "$state_root/backups" "$state_root/sync"

cat > "$fake_bin/systemctl" <<'EOF'
#!/bin/sh
set -eu
state=${FFDB_TEST_SERVICE_STATE:?}
case "${1:-}" in
  is-active)
    shift
    [ "${1:-}" = --quiet ] && shift
    grep -F -x -q "${1:?}" "$state"
    ;;
  stop)
    shift
    for service in "$@"; do
      awk -v service="$service" '$0 != service' "$state" > "$state.next"
      mv "$state.next" "$state"
    done
    ;;
  start)
    shift
    for service in "$@"; do
      grep -F -x -q "$service" "$state" || printf '%s\n' "$service" >> "$state"
    done
    ;;
  *) exit 2 ;;
esac
EOF

cat > "$fake_bin/pg_dump" <<'EOF'
#!/bin/sh
set -eu
output=
for argument in "$@"; do
  case "$argument" in --file=*) output=${argument#--file=} ;; esac
done
[ -n "$output" ] || exit 2
printf 'PGDMP test control-plane dump\n' > "$output"
printf '%s\n' pg_dump >> "${FFDB_TEST_COMMAND_LOG:?}"
EOF

cat > "$fake_bin/pg_restore" <<'EOF'
#!/bin/sh
set -eu
if [ "${1:-}" = --list ]; then
  grep -F -q PGDMP "${2:?}"
  printf '%s\n' pg_restore-list >> "${FFDB_TEST_COMMAND_LOG:?}"
  exit 0
fi
printf '%s\n' pg_restore-transactional >> "${FFDB_TEST_COMMAND_LOG:?}"
exit 0
EOF

cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
set -eu
last=
for argument in "$@"; do last=$argument; done
printf '%s\n' "curl $last" >> "${FFDB_TEST_COMMAND_LOG:?}"
EOF

chmod 0755 "$fake_bin/systemctl" "$fake_bin/pg_dump" \
  "$fake_bin/pg_restore" "$fake_bin/curl"
sed -e 's|@VERSION@|0.1.0|g' "$ROOT_DIR/infra/release/ffdb-backup.in" \
  > "$test_root/ffdb-backup"
chmod 0755 "$test_root/ffdb-backup"

sqlite3 "$state_root/projects/project.sqlite3" \
  'CREATE TABLE records(value TEXT NOT NULL); INSERT INTO records VALUES ("original-project");'
sqlite3 "$state_root/metrics/org.sqlite3" \
  'CREATE TABLE usage(value TEXT NOT NULL); INSERT INTO usage VALUES ("original-usage");'
printf '%s\n' encrypted-project-backup > "$state_root/backups/backup.ffdb"
printf '%s\n' sync-checkpoint > "$state_root/sync/checkpoint"
cat > "$config_file" <<EOF
FFDB_DATABASE_URL=postgres://ffdb:$secret@postgres.test:5432/ffdb
FFDB_S3_PUBLIC_ORIGIN=https://objects.ffdb.test
EOF
printf '%s\n' 0.1.0 > "$version_file"
printf '%s\n' ffdb-api.service ffdb-sync-worker.service ffdb-gateway.service > "$service_state"

export FFDB_ALLOW_UNPRIVILEGED=1
export FFDB_BACKUP_MODE=native
export FFDB_NATIVE_CONFIG_FILE=$config_file
export FFDB_NATIVE_STATE_ROOT=$state_root
export FFDB_NATIVE_VERSION_FILE=$version_file
export FFDB_NATIVE_OWNER=$(id -un)
export FFDB_NATIVE_GROUP=$(id -gn)
export FFDB_NATIVE_CONFIG_OWNER=$FFDB_NATIVE_OWNER
export FFDB_NATIVE_CONFIG_GROUP=$FFDB_NATIVE_GROUP
export FFDB_SYSTEMCTL_COMMAND=$fake_bin/systemctl
export FFDB_CURL_COMMAND=$fake_bin/curl
export FFDB_PG_DUMP_COMMAND=$fake_bin/pg_dump
export FFDB_PG_RESTORE_COMMAND=$fake_bin/pg_restore
export FFDB_SQLITE3_COMMAND=$(command -v sqlite3)
export FFDB_TEST_SERVICE_STATE=$service_state
export FFDB_TEST_COMMAND_LOG=$command_log

archive=$test_root/host-backup.tar.gz
create_log=$test_root/create.log
"$test_root/ffdb-backup" create "$archive" > "$create_log" 2>&1
[ -f "$archive" ] || { cat "$create_log" >&2; exit 1; }
if [ "$(uname -s)" = Darwin ]; then
  [ "$(stat -f '%Lp' "$archive")" = 600 ]
else
  [ "$(stat -c '%a' "$archive")" = 600 ]
fi
for required_member in manifest.env SHA256SUMS config/ffdb.env postgres.dump \
  volumes/projects.tar volumes/metrics.tar volumes/backups.tar volumes/sync.tar; do
  tar -tzf "$archive" | grep -F -q "ffdb-host-backup/$required_member"
done
tar -xOf "$archive" ffdb-host-backup/manifest.env \
  | grep -F -q 'FFDB_HOST_BACKUP_PROFILE=native'
grep -F -x -q ffdb-api.service "$service_state"
grep -F -x -q ffdb-sync-worker.service "$service_state"
! grep -F -q "$secret" "$create_log"
if "$test_root/ffdb-backup" create "$archive" >/dev/null 2>&1; then
  printf '%s\n' "backup create unexpectedly overwrote an archive" >&2
  exit 1
fi

sqlite3 "$state_root/projects/project.sqlite3" \
  'DELETE FROM records; INSERT INTO records VALUES ("mutated-project");'
sqlite3 "$state_root/metrics/org.sqlite3" \
  'DELETE FROM usage; INSERT INTO usage VALUES ("mutated-usage");'
printf '%s\n' 'FFDB_DATABASE_URL=postgres://changed.invalid/ffdb' > "$config_file"
"$fake_bin/systemctl" stop ffdb-api.service ffdb-sync-worker.service
if "$test_root/ffdb-backup" restore "$archive" >/dev/null 2>&1; then
  printf '%s\n' "backup restore unexpectedly proceeded without --yes" >&2
  exit 1
fi
[ "$(sqlite3 "$state_root/projects/project.sqlite3" 'SELECT value FROM records;')" = mutated-project ]

corrupt_dir=$test_root/corrupt
install -d "$corrupt_dir"
tar -xzf "$archive" -C "$corrupt_dir"
printf '%s\n' corruption >> "$corrupt_dir/ffdb-host-backup/postgres.dump"
corrupt_archive=$test_root/corrupt.tar.gz
tar -czf "$corrupt_archive" -C "$corrupt_dir" ffdb-host-backup
if "$test_root/ffdb-backup" restore "$corrupt_archive" --yes >/dev/null 2>&1; then
  printf '%s\n' "backup restore unexpectedly accepted a checksum mismatch" >&2
  exit 1
fi
[ "$(sqlite3 "$state_root/projects/project.sqlite3" 'SELECT value FROM records;')" = mutated-project ]
grep -F -q changed.invalid "$config_file"

restore_log=$test_root/restore.log
"$test_root/ffdb-backup" restore "$archive" --yes > "$restore_log" 2>&1
[ "$(sqlite3 "$state_root/projects/project.sqlite3" 'SELECT value FROM records;')" = original-project ]
[ "$(sqlite3 "$state_root/metrics/org.sqlite3" 'SELECT value FROM usage;')" = original-usage ]
grep -F -q "$secret" "$config_file"
! grep -F -q "$secret" "$restore_log"
grep -F -x -q ffdb-api.service "$service_state"
grep -F -x -q ffdb-sync-worker.service "$service_state"
grep -F -x -q pg_restore-list "$command_log"
grep -F -x -q pg_restore-transactional "$command_log"
grep -F -q 'http://127.0.0.1:8080/readyz' "$command_log"
grep -F -q 'http://127.0.0.1:5173/readyz' "$command_log"

printf '%s\n' "host backup create/restore tests passed"
