#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/ffdb-compose-backup-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
fake_bin=$test_root/fake-bin
release_dir=$test_root/release
volume_root=$test_root/volumes
service_state=$test_root/services
command_log=$test_root/commands.log
config_file=$test_root/single-host.env
secret=compose-database-password-must-not-be-logged
install -d "$fake_bin" "$release_dir" "$volume_root"
for logical in postgres-data project-data metrics-data backup-data sync-data \
  minio-data mailpit-data; do
  install -d "$volume_root/$logical"
done

cat > "$fake_bin/docker" <<'EOF'
#!/bin/sh
set -eu
state=${FFDB_TEST_COMPOSE_STATE:?}
volumes=${FFDB_TEST_COMPOSE_VOLUMES:?}
log=${FFDB_TEST_COMMAND_LOG:?}
all_services='postgres
minio
minio-bootstrap
mailpit
api
sync-worker
gateway'

remove_service() {
  awk -v service="$1" '$0 != service' "$state" > "$state.next"
  mv "$state.next" "$state"
}

if [ "${1:-}" = volume ] && [ "${2:-}" = ls ]; then
  logical=
  for argument in "$@"; do
    case "$argument" in
      label=com.docker.compose.volume=*) logical=${argument##*=} ;;
    esac
  done
  [ -n "$logical" ] || exit 2
  printf 'ffdb-single-host_%s\n' "$logical"
  exit 0
fi

if [ "${1:-}" = compose ]; then
  shift
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --env-file|-f) shift 2 ;;
      *) break ;;
    esac
  done
  operation=${1:-}
  [ -n "$operation" ] || exit 2
  shift
  case "$operation" in
    ps)
      cat "$state"
      ;;
    stop)
      for service in "$@"; do remove_service "$service"; done
      ;;
    exec)
      [ "${1:-}" = -T ] && shift
      service=${1:?}
      shift
      case "${1:-}" in
        pg_dump)
          [ "${FFDB_TEST_FAIL_PG_DUMP:-0}" != 1 ] || exit 71
          printf 'PGDMP packaged control-plane dump\n'
          printf '%s\n' compose-pg-dump >> "$log"
          ;;
        pg_restore)
          grep -F -q PGDMP
          printf '%s\n' compose-pg-restore-transactional >> "$log"
          ;;
        *) exit 2 ;;
      esac
      ;;
    config)
      exit 0
      ;;
    down)
      : > "$state"
      for argument in "$@"; do
        if [ "$argument" = --volumes ]; then
          for directory in "$volumes"/*; do
            find "$directory" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
          done
        fi
      done
      ;;
    create)
      for logical in postgres-data project-data metrics-data backup-data sync-data \
        minio-data mailpit-data; do install -d "$volumes/$logical"; done
      ;;
    up)
      postgres_only=0
      for argument in "$@"; do [ "$argument" != postgres ] || postgres_only=1; done
      if [ "$postgres_only" -eq 1 ]; then
        printf '%s\n' postgres > "$state"
      else
        printf '%s\n' "$all_services" > "$state"
      fi
      ;;
    *) exit 2 ;;
  esac
  exit 0
fi

if [ "${1:-}" = run ]; then
  shift
  entrypoint=
  source_path=
  destination_path=
  check_path=
  project_check=
  metrics_check=
  backup_path=
  command_text=
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --entrypoint) entrypoint=$2; shift 2 ;;
      --volume)
        specification=$2
        host_source=${specification%%:*}
        remainder=${specification#*:}
        mount_target=${remainder%%:*}
        case "$host_source" in
          /*) resolved=$host_source ;;
          ffdb-single-host_*) resolved=$volumes/${host_source#ffdb-single-host_} ;;
          *) exit 2 ;;
        esac
        case "$mount_target" in
          /source) source_path=$resolved ;;
          /destination) destination_path=$resolved ;;
          /backup) backup_path=$resolved ;;
          /check) check_path=$resolved ;;
          /check/project-data) project_check=$resolved ;;
          /check/metrics-data) metrics_check=$resolved ;;
        esac
        shift 2
        ;;
      -ec) command_text=$2; shift 2 ;;
      *) shift ;;
    esac
  done
  if [ "$entrypoint" = pg_restore ]; then
    grep -F -q PGDMP "$backup_path/postgres.dump"
    printf '%s\n' compose-pg-restore-list >> "$log"
  elif [ -n "$source_path" ] && [ -n "$destination_path" ]; then
    case "$command_text" in
      *'tar -cpf /destination/'*)
        filename=${command_text##*/destination/}
        filename=${filename%% *}
        tar -cpf "$destination_path/$filename" -C "$source_path" .
        ;;
      *'tar -xpf /source/'*)
        filename=${command_text##*/source/}
        filename=${filename%% *}
        tar -xpf "$source_path/$filename" -C "$destination_path"
        ;;
      *) exit 2 ;;
    esac
  elif [ -n "$check_path" ]; then
    find "$check_path/project-data" "$check_path/metrics-data" -type f -name '*.sqlite3' -print |
      while IFS= read -r database; do
        [ "$(sqlite3 "$database" 'PRAGMA quick_check(1);')" = ok ] || exit 1
      done
  elif [ -n "$project_check" ] && [ -n "$metrics_check" ]; then
    find "$project_check" "$metrics_check" -type f -name '*.sqlite3' -print |
      while IFS= read -r database; do
        [ "$(sqlite3 "$database" 'PRAGMA quick_check(1);')" = ok ] || exit 1
      done
  else
    exit 2
  fi
  exit 0
fi

exit 2
EOF

cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
set -eu
last=
for argument in "$@"; do last=$argument; done
printf '%s\n' "curl $last" >> "${FFDB_TEST_COMMAND_LOG:?}"
EOF

chmod 0755 "$fake_bin/docker" "$fake_bin/curl"
sed -e 's|@VERSION@|0.1.0|g' "$ROOT_DIR/infra/release/ffdb-backup.in" \
  > "$test_root/ffdb-backup"
chmod 0755 "$test_root/ffdb-backup"
cp "$ROOT_DIR/infra/release/compose.single-host.yaml" "$release_dir/compose.single-host.yaml"
cat > "$release_dir/release.env" <<'EOF'
FFDB_RELEASE_VERSION=0.1.0
FFDB_RUNTIME_IMAGE=registry.test/ffdb-runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
FFDB_POSTGRES_IMAGE=postgres@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
EOF
printf '%s\n' 0.1.0 > "$release_dir/VERSION"
cat > "$config_file" <<EOF
FFDB_GATEWAY_PORT=5173
FFDB_DATABASE_URL=postgres://ffdb:$secret@postgres:5432/ffdb
EOF

sqlite3 "$volume_root/project-data/project.sqlite3" \
  'CREATE TABLE records(value TEXT NOT NULL); INSERT INTO records VALUES ("compose-project");'
sqlite3 "$volume_root/metrics-data/org.sqlite3" \
  'CREATE TABLE usage(value TEXT NOT NULL); INSERT INTO usage VALUES ("compose-usage");'
printf '%s\n' encrypted-backup > "$volume_root/backup-data/backup.ffdb"
printf '%s\n' sync-checkpoint > "$volume_root/sync-data/checkpoint"
printf '%s\n' object-bytes > "$volume_root/minio-data/object"
printf '%s\n' captured-mail > "$volume_root/mailpit-data/message"
printf '%s\n' postgres minio minio-bootstrap mailpit api sync-worker gateway > "$service_state"

export FFDB_ALLOW_UNPRIVILEGED=1
export FFDB_BACKUP_MODE=compose
export FFDB_COMPOSE_RELEASE_DIR=$release_dir
export FFDB_COMPOSE_CONFIG_FILE=$config_file
export FFDB_COMPOSE_FILENAME=compose.single-host.yaml
export FFDB_COMPOSE_PROFILE=single-host
export FFDB_DOCKER_COMMAND=$fake_bin/docker
export FFDB_CURL_COMMAND=$fake_bin/curl
export FFDB_TEST_COMPOSE_STATE=$service_state
export FFDB_TEST_COMPOSE_VOLUMES=$volume_root
export FFDB_TEST_COMMAND_LOG=$command_log

failure_archive=$test_root/failure.tar.gz
if FFDB_TEST_FAIL_PG_DUMP=1 "$test_root/ffdb-backup" create "$failure_archive" \
  >/dev/null 2>&1; then
  printf '%s\n' "packaged backup unexpectedly survived an injected dump failure" >&2
  exit 1
fi
[ ! -e "$failure_archive" ]
grep -F -x -q api "$service_state"
grep -F -x -q sync-worker "$service_state"

archive=$test_root/compose-backup.tar.gz
create_log=$test_root/create.log
if ! "$test_root/ffdb-backup" create "$archive" > "$create_log" 2>&1; then
  cat "$create_log" >&2
  exit 1
fi
[ -f "$archive" ]
grep -F -x -q gateway "$service_state"
! grep -F -q "$secret" "$create_log"

sqlite3 "$volume_root/project-data/project.sqlite3" \
  'DELETE FROM records; INSERT INTO records VALUES ("changed-project");'
sqlite3 "$volume_root/metrics-data/org.sqlite3" \
  'DELETE FROM usage; INSERT INTO usage VALUES ("changed-usage");'
printf '%s\n' changed-object > "$volume_root/minio-data/object"
printf '%s\n' 'FFDB_DATABASE_URL=postgres://changed.invalid/ffdb' > "$config_file"
: > "$service_state"
restore_log=$test_root/restore.log
"$test_root/ffdb-backup" restore "$archive" --yes > "$restore_log" 2>&1
[ "$(sqlite3 "$volume_root/project-data/project.sqlite3" 'SELECT value FROM records;')" = compose-project ]
[ "$(sqlite3 "$volume_root/metrics-data/org.sqlite3" 'SELECT value FROM usage;')" = compose-usage ]
[ "$(sed -n '1p' "$volume_root/minio-data/object")" = object-bytes ]
grep -F -q "$secret" "$config_file"
! grep -F -q "$secret" "$restore_log"
grep -F -x -q compose-pg-restore-list "$command_log"
grep -F -x -q compose-pg-restore-transactional "$command_log"
grep -F -q 'http://127.0.0.1:5173/readyz' "$command_log"
grep -F -x -q gateway "$service_state"

printf '%s\n' "packaged Compose host backup create/restore tests passed"
