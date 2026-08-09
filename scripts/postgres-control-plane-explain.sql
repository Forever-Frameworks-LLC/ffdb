\set ON_ERROR_STOP on
\pset pager off
\echo 'FFDB rollback-only control-plane index benchmark (' :bench_rows ' rows per path)'

BEGIN;
SET LOCAL max_parallel_workers_per_gather = 0;

CREATE TEMP TABLE bench_memberships (
    organization_id bigint NOT NULL,
    user_id bigint NOT NULL,
    PRIMARY KEY (organization_id, user_id)
) ON COMMIT DROP;
INSERT INTO bench_memberships
SELECT 1, value FROM generate_series(1, :bench_rows) AS value;

CREATE TEMP TABLE bench_audit_events (
    append_sequence bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    organization_id bigint,
    project_id bigint,
    event_hash bytea NOT NULL
) ON COMMIT DROP;
INSERT INTO bench_audit_events (organization_id, project_id, event_hash)
VALUES (1, NULL, decode(repeat('00', 32), 'hex'));
INSERT INTO bench_audit_events (organization_id, project_id, event_hash)
SELECT 2 + (value % 99), NULL, decode(repeat('00', 32), 'hex')
FROM generate_series(1, :bench_rows) AS value;

CREATE TEMP TABLE bench_backups (
    id bigint PRIMARY KEY,
    project_id bigint NOT NULL,
    state text NOT NULL,
    created_at timestamptz NOT NULL
) ON COMMIT DROP;
INSERT INTO bench_backups
SELECT value, 1, 'ready', now() - value * interval '1 second'
FROM generate_series(1, :bench_rows) AS value;

CREATE TEMP TABLE bench_auth_sessions (
    id bigint PRIMARY KEY,
    project_id bigint NOT NULL,
    user_id bigint NOT NULL,
    created_at timestamptz NOT NULL
) ON COMMIT DROP;
INSERT INTO bench_auth_sessions
SELECT value, 1, 1, now() - value * interval '1 second'
FROM generate_series(1, :bench_rows) AS value;

CREATE TEMP TABLE bench_order_lines (
    id bigint PRIMARY KEY,
    project_id bigint NOT NULL,
    order_id bigint NOT NULL,
    product_id bigint NOT NULL
) ON COMMIT DROP;
INSERT INTO bench_order_lines
SELECT value, 1, 1 + ((value - 1) * 100 / :bench_rows), 1
FROM generate_series(1, :bench_rows) AS value;

ANALYZE bench_memberships;
ANALYZE bench_audit_events;
ANALYZE bench_backups;
ANALYZE bench_auth_sessions;
ANALYZE bench_order_lines;

\echo 'BEFORE: membership lookup by user'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT organization_id FROM bench_memberships WHERE user_id = :bench_rows;

\echo 'BEFORE: organization audit-chain head'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT event_hash FROM bench_audit_events
WHERE project_id IS NULL AND organization_id = 1
ORDER BY append_sequence DESC LIMIT 1;

\echo 'BEFORE: project backup history'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT id FROM bench_backups
WHERE project_id = 1 AND state <> 'deleted'
ORDER BY created_at DESC LIMIT 200;

\echo 'BEFORE: user auth-session history'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT id FROM bench_auth_sessions
WHERE project_id = 1 AND user_id = 1
ORDER BY created_at DESC LIMIT 100;

\echo 'BEFORE: one order\'s lines among 100 orders'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT product_id FROM bench_order_lines
WHERE project_id = 1 AND order_id = 100
ORDER BY id;

CREATE INDEX bench_memberships_user_idx
    ON bench_memberships (user_id, organization_id);
CREATE INDEX bench_audit_organization_sequence_idx
    ON bench_audit_events (organization_id, append_sequence DESC)
    WHERE project_id IS NULL AND organization_id IS NOT NULL;
CREATE INDEX bench_backups_project_created_idx
    ON bench_backups (project_id, created_at DESC)
    WHERE state <> 'deleted';
CREATE INDEX bench_auth_sessions_user_created_idx
    ON bench_auth_sessions (project_id, user_id, created_at DESC);
CREATE INDEX bench_order_lines_order_idx
    ON bench_order_lines (project_id, order_id, id);

ANALYZE bench_memberships;
ANALYZE bench_audit_events;
ANALYZE bench_backups;
ANALYZE bench_auth_sessions;
ANALYZE bench_order_lines;

\echo 'AFTER: membership lookup by user'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT organization_id FROM bench_memberships WHERE user_id = :bench_rows;

\echo 'AFTER: organization audit-chain head'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT event_hash FROM bench_audit_events
WHERE project_id IS NULL AND organization_id = 1
ORDER BY append_sequence DESC LIMIT 1;

\echo 'AFTER: project backup history'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT id FROM bench_backups
WHERE project_id = 1 AND state <> 'deleted'
ORDER BY created_at DESC LIMIT 200;

\echo 'AFTER: user auth-session history'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT id FROM bench_auth_sessions
WHERE project_id = 1 AND user_id = 1
ORDER BY created_at DESC LIMIT 100;

\echo 'AFTER: one order\'s lines among 100 orders'
EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)
SELECT product_id FROM bench_order_lines
WHERE project_id = 1 AND order_id = 100
ORDER BY id;

ROLLBACK;
\echo 'Rollback complete; no benchmark rows or indexes were retained.'
