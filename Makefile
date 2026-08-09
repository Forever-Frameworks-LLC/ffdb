SHELL := /bin/sh

CARGO ?= cargo
PNPM ?= pnpm
NODE ?= node
DOCKER_COMPOSE ?= docker compose

FFDB_LOAD_REQUESTS ?= 300
FFDB_LOAD_CONCURRENCY ?= 12
FFDB_LOAD_WARMUP ?= 12
FFDB_LOAD_TIMEOUT_MS ?= 2000
FFDB_LOAD_GATEWAY_URL ?= http://127.0.0.1:5173/healthz
FFDB_LOAD_API_URL ?= http://127.0.0.1:8080/healthz
FFDB_LOAD_READY_URL ?= http://127.0.0.1:5173/readyz
LOAD_TEST_ARGS := --requests "$(FFDB_LOAD_REQUESTS)" --concurrency "$(FFDB_LOAD_CONCURRENCY)" \
	--warmup "$(FFDB_LOAD_WARMUP)" --timeout-ms "$(FFDB_LOAD_TIMEOUT_MS)" \
	$(if $(FFDB_LOAD_MAX_P95_MS),--max-p95-ms "$(FFDB_LOAD_MAX_P95_MS)",)

FFDB_QUERY_LOAD_REQUESTS ?= 100
FFDB_QUERY_LOAD_CONCURRENCY ?= 4
FFDB_QUERY_LOAD_WARMUP ?= 4
FFDB_QUERY_LOAD_TIMEOUT_MS ?= 5000
QUERY_LOAD_TEST_ARGS := --requests "$(FFDB_QUERY_LOAD_REQUESTS)" \
	--concurrency "$(FFDB_QUERY_LOAD_CONCURRENCY)" \
	--warmup "$(FFDB_QUERY_LOAD_WARMUP)" \
	--timeout-ms "$(FFDB_QUERY_LOAD_TIMEOUT_MS)" \
	$(if $(FFDB_QUERY_LOAD_MAX_P95_MS),--max-p95-ms "$(FFDB_QUERY_LOAD_MAX_P95_MS)",)

FFDB_PG_BENCH_ROWS ?= 50000
FFDB_PG_BENCH_SECONDS ?= 5
FFDB_PG_BENCH_CONCURRENCY ?= 4

TS_BUILD_OUTPUTS := \
	apps/landing/dist \
	apps/landing/tsconfig.tsbuildinfo \
	apps/docs/dist \
	apps/docs/tsconfig.tsbuildinfo \
	apps/portal/dist \
	apps/portal/tsconfig.app.tsbuildinfo \
	apps/portal/tsconfig.node.tsbuildinfo \
	packages/cli/dist \
	packages/client/dist \
	packages/email-components/dist \
	packages/react-native/dist \
	packages/react/dist \
	packages/sync-client/dist

.DEFAULT_GOAL := help

.PHONY: \
	bootstrap build rust-build typescript-build check test live verify format \
	compose-rebuild compose-fresh production-config-check release-check release-bundle sdk-packages \
	native-install-linux-test \
	github-security-audit github-security-apply \
	distribution-check load-test load-test-gateway load-test-api load-test-ready load-test-query load-test-check \
	postgres-bench status clean dev-up dev-down infra-up help

help:
	@echo "FFDB developer commands"
	@echo "  make bootstrap        install locked dependencies and validate Compose"
	@echo "  make build            build every Rust and TypeScript target"
	@echo "  make check            run formatting, lint, types, docs, and Compose checks"
	@echo "  make test             run every Rust and TypeScript test"
	@echo "  make live             build all three web apps, rebuild containers, run live E2E"
	@echo "  make load-test        run bounded loopback gateway, Axum, and readiness load smokes"
	@echo "  make load-test-query  run opt-in authenticated, read-only project query load smoke"
	@echo "  make load-test-check  test the load harness without a running FFDB stack"
	@echo "  make postgres-bench   run bounded local pgbench plus rollback-only control-plane EXPLAINs"
	@echo "  make compose-rebuild  rebuild/recreate the full stack from this checkout"
	@echo "  make compose-fresh    destroy local FFDB volumes and start first-run setup"
	@echo "  make production-config-check  validate the production Compose model"
	@echo "  make release-check     validate versioned host distribution scripts/bundles"
	@echo "  make native-install-linux-test  validate the native installer in disposable Linux containers"
	@echo "  make release-bundle    build bundle (requires FFDB_VERSION and image digests)"
	@echo "  make sdk-packages      build six version-matched publishable npm tarballs"
	@echo "  make github-security-audit  verify hosted GitHub repository security controls"
	@echo "  make github-security-apply  apply and verify hosted GitHub repository security controls"
	@echo "  make distribution-check  verify the public curl installation channel"
	@echo "  make status           show Compose containers and image identities"
	@echo "  make clean            stop Compose and remove build outputs; retain all data"

bootstrap:
	@command -v $(CARGO) >/dev/null
	@command -v $(PNPM) >/dev/null
	@command -v docker >/dev/null
	@test -f .env || cp .env.example .env
	$(CARGO) fetch --locked
	$(PNPM) install --frozen-lockfile
	$(DOCKER_COMPOSE) config --quiet

build: rust-build typescript-build

rust-build:
	$(CARGO) build --locked --workspace --all-targets

typescript-build:
	$(PNPM) build

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --locked --workspace --all-targets --all-features -- -D warnings
	$(PNPM) check
	$(PNPM) lint
	$(PNPM) docs:check
	$(MAKE) load-test-check
	$(DOCKER_COMPOSE) config --quiet
	$(MAKE) production-config-check
	$(MAKE) release-check

test:
	$(CARGO) test --locked --workspace --all-targets --all-features
	$(PNPM) test

# Dockerfiles COPY the current checkout into their build stages. --build ensures
# those contexts are evaluated, and --force-recreate prevents a running service
# from continuing to use an older image. Named volumes are intentionally retained.
compose-rebuild:
	$(DOCKER_COMPOSE) up --detach --build --force-recreate --wait

# This is the explicit local first-install acceptance path. It is deliberately
# guarded because it destroys this Compose project's PostgreSQL, object, mail,
# project, backup, metrics, and sync volumes before starting the current build.
compose-fresh:
	@test "$(FFDB_CONFIRM_FRESH)" = "DELETE_LOCAL_FFDB_DATA" || { \
		echo "Refusing to delete local FFDB volumes." >&2; \
		echo "Run: FFDB_CONFIRM_FRESH=DELETE_LOCAL_FFDB_DATA make compose-fresh" >&2; \
		exit 2; \
	}
	$(DOCKER_COMPOSE) down --volumes --remove-orphans
	$(DOCKER_COMPOSE) up --detach --build --force-recreate --wait

production-config-check:
	$(DOCKER_COMPOSE) --env-file infra/docker/production.env.example \
		-f compose.production.yaml config --quiet

release-check:
	node scripts/check-release-version.mjs $$(node -p 'require("./package.json").version')
	node scripts/check-sdk-package-contract.mjs $$(node -p 'require("./package.json").version')
	@for script in infra/release/install.sh infra/release/uninstall.sh \
		infra/release/ffdb-host.in infra/release/ffdb-backup.in \
		infra/release/native/install-native.sh \
		infra/release/native/uninstall-native.sh scripts/build-release-bundle.sh \
		scripts/build-sdk-packages.sh scripts/check-public-distribution.sh \
		scripts/build-native-bundle.sh scripts/test-release-distribution.sh \
		scripts/test-host-backup.sh scripts/test-host-backup-compose.sh \
		scripts/test-native-install-linux.sh \
		scripts/test-native-install-container.sh \
		scripts/configure-github-security.sh; do \
		sh -n "$$script" || exit; \
	done
	scripts/test-host-backup.sh
	scripts/test-host-backup-compose.sh
	scripts/test-release-distribution.sh

release-bundle:
	@test -n "$(FFDB_VERSION)" || { echo "FFDB_VERSION is required" >&2; exit 2; }
	@test -n "$(FFDB_RUNTIME_IMAGE)" || { echo "FFDB_RUNTIME_IMAGE@sha256 is required" >&2; exit 2; }
	@test -n "$(FFDB_GATEWAY_IMAGE)" || { echo "FFDB_GATEWAY_IMAGE@sha256 is required" >&2; exit 2; }

	@test -n "$(FFDB_POSTGRES_IMAGE)" || { echo "FFDB_POSTGRES_IMAGE@sha256 is required" >&2; exit 2; }
	@test -n "$(FFDB_MINIO_IMAGE)" || { echo "FFDB_MINIO_IMAGE@sha256 is required" >&2; exit 2; }
	@test -n "$(FFDB_MAILPIT_IMAGE)" || { echo "FFDB_MAILPIT_IMAGE@sha256 is required" >&2; exit 2; }
	FFDB_POSTGRES_IMAGE="$(FFDB_POSTGRES_IMAGE)" \
	FFDB_MINIO_IMAGE="$(FFDB_MINIO_IMAGE)" \
	FFDB_MAILPIT_IMAGE="$(FFDB_MAILPIT_IMAGE)" \
	scripts/build-release-bundle.sh "$(FFDB_VERSION)" "$(FFDB_RUNTIME_IMAGE)" \
		"$(FFDB_GATEWAY_IMAGE)" "$(or $(FFDB_RELEASE_OUTPUT_DIR),dist/release)"

native-install-linux-test:
	scripts/test-native-install-linux.sh

sdk-packages:
	scripts/build-sdk-packages.sh $$(node -p 'require("./package.json").version') \
		"$(or $(FFDB_SDK_OUTPUT_DIR),dist/sdk)"

github-security-audit:
	scripts/configure-github-security.sh --audit

github-security-apply:
	scripts/configure-github-security.sh --apply

distribution-check:
	scripts/check-public-distribution.sh "$(or $(FFDB_GITHUB_RELEASES_URL),https://github.com/Forever-Frameworks-LLC/ffdb/releases)"

live: typescript-build compose-rebuild
	$(PNPM) test:live

# These fixed-count, loopback-only GET profiles are safe for an existing local
# stack. They never create or mutate FFDB resources. Override the FFDB_LOAD_*
# variables to change the bounded sample within the harness's hard caps.
load-test: load-test-gateway load-test-api load-test-ready

load-test-gateway:
	$(NODE) scripts/load-smoke.mjs --url "$(FFDB_LOAD_GATEWAY_URL)" $(LOAD_TEST_ARGS)

load-test-api:
	$(NODE) scripts/load-smoke.mjs --url "$(FFDB_LOAD_API_URL)" $(LOAD_TEST_ARGS)

load-test-ready:
	$(NODE) scripts/load-smoke.mjs --url "$(FFDB_LOAD_READY_URL)" $(LOAD_TEST_ARGS)

# This target is intentionally excluded from load-test. Although its SQL is the
# hardcoded read-only SELECT 1 probe, it consumes metering/rate-limit budget and
# writes audit/observability state. Project ID and token are environment-only.
load-test-query:
	$(NODE) scripts/query-load-smoke.mjs $(QUERY_LOAD_TEST_ARGS)

load-test-check:
	$(NODE) --test scripts/load-smoke.test.mjs scripts/query-load-smoke.test.mjs

# Local PostgreSQL diagnostics are intentionally separate from the authenticated
# HTTP harness. Synthetic EXPLAIN data lives only in temporary tables inside a
# transaction that always rolls back; the pgbench script executes SELECT 1.
postgres-bench:
	@case "$(FFDB_PG_BENCH_ROWS)" in ''|*[!0-9]*) echo "FFDB_PG_BENCH_ROWS must be an integer" >&2; exit 2;; esac
	@case "$(FFDB_PG_BENCH_SECONDS)" in ''|*[!0-9]*) echo "FFDB_PG_BENCH_SECONDS must be an integer" >&2; exit 2;; esac
	@case "$(FFDB_PG_BENCH_CONCURRENCY)" in ''|*[!0-9]*) echo "FFDB_PG_BENCH_CONCURRENCY must be an integer" >&2; exit 2;; esac
	@test "$(FFDB_PG_BENCH_ROWS)" -ge 1000 && test "$(FFDB_PG_BENCH_ROWS)" -le 50000 || { echo "FFDB_PG_BENCH_ROWS must be 1000..50000" >&2; exit 2; }
	@test "$(FFDB_PG_BENCH_SECONDS)" -ge 1 && test "$(FFDB_PG_BENCH_SECONDS)" -le 30 || { echo "FFDB_PG_BENCH_SECONDS must be 1..30" >&2; exit 2; }
	@test "$(FFDB_PG_BENCH_CONCURRENCY)" -ge 1 && test "$(FFDB_PG_BENCH_CONCURRENCY)" -le 32 || { echo "FFDB_PG_BENCH_CONCURRENCY must be 1..32" >&2; exit 2; }
	@$(DOCKER_COMPOSE) ps --status running --services | rg -x postgres >/dev/null || { echo "local Compose PostgreSQL is not running" >&2; exit 2; }
	$(DOCKER_COMPOSE) exec -T postgres pgbench --no-vacuum --protocol=prepared \
		--client="$(FFDB_PG_BENCH_CONCURRENCY)" --jobs=1 --time="$(FFDB_PG_BENCH_SECONDS)" \
		--file=/dev/stdin --username=ffdb ffdb < scripts/postgres-select.pgbench.sql
	$(DOCKER_COMPOSE) exec -T postgres psql --username=ffdb --dbname=ffdb --no-psqlrc \
		--set=ON_ERROR_STOP=1 --set=bench_rows="$(FFDB_PG_BENCH_ROWS)" \
		--file=/dev/stdin < scripts/postgres-control-plane-explain.sql

status:
	$(DOCKER_COMPOSE) ps --all
	$(DOCKER_COMPOSE) images

infra-up:
	$(DOCKER_COMPOSE) up --detach --wait postgres minio minio-bootstrap mailpit

dev-up: compose-rebuild

dev-down:
	$(DOCKER_COMPOSE) down --remove-orphans

format:
	$(CARGO) fmt --all
	$(PNPM) format

verify: check test build

# This target deliberately omits `docker compose down --volumes`, the data/
# directory, node_modules, and the pnpm store. Only reproducible build outputs
# are removed after stopping containers.
clean: dev-down
	$(CARGO) clean
	rm -rf $(TS_BUILD_OUTPUTS) fuzz/target
