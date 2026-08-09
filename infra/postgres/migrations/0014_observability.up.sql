CREATE TABLE observability_http_buckets (
    bucket_start_ms bigint NOT NULL CHECK (bucket_start_ms >= 0 AND bucket_start_ms % 60000 = 0),
    project_id uuid NOT NULL,
    method text NOT NULL CHECK (char_length(method) BETWEEN 1 AND 12),
    route text NOT NULL CHECK (char_length(route) BETWEEN 1 AND 256),
    status_class smallint NOT NULL CHECK (status_class BETWEEN 1 AND 5),
    request_count bigint NOT NULL DEFAULT 0 CHECK (request_count >= 0),
    duration_sum_ms double precision NOT NULL DEFAULT 0 CHECK (duration_sum_ms >= 0),
    duration_max_ms double precision NOT NULL DEFAULT 0 CHECK (duration_max_ms >= 0),
    latency_le_5_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_5_ms >= 0),
    latency_le_10_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_10_ms >= 0),
    latency_le_25_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_25_ms >= 0),
    latency_le_50_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_50_ms >= 0),
    latency_le_100_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_100_ms >= 0),
    latency_le_250_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_250_ms >= 0),
    latency_le_500_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_500_ms >= 0),
    latency_le_1000_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_1000_ms >= 0),
    latency_le_2500_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_2500_ms >= 0),
    latency_le_5000_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_5000_ms >= 0),
    latency_le_15000_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_15000_ms >= 0),
    latency_le_60000_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_60000_ms >= 0),
    PRIMARY KEY (bucket_start_ms, project_id, method, route, status_class)
);

CREATE INDEX observability_http_project_time_idx
    ON observability_http_buckets (project_id, bucket_start_ms DESC);
CREATE INDEX observability_http_time_idx
    ON observability_http_buckets (bucket_start_ms DESC);

CREATE TABLE observability_query_buckets (
    bucket_start_ms bigint NOT NULL CHECK (bucket_start_ms >= 0 AND bucket_start_ms % 60000 = 0),
    project_id uuid NOT NULL,
    fingerprint text NOT NULL CHECK (char_length(fingerprint) = 64),
    shape text NOT NULL CHECK (char_length(shape) BETWEEN 1 AND 320),
    statement_kind text NOT NULL CHECK (char_length(statement_kind) BETWEEN 1 AND 32),
    read_only boolean NOT NULL,
    execution_count bigint NOT NULL DEFAULT 0 CHECK (execution_count >= 0),
    error_count bigint NOT NULL DEFAULT 0 CHECK (error_count >= 0 AND error_count <= execution_count),
    duration_sum_ms double precision NOT NULL DEFAULT 0 CHECK (duration_sum_ms >= 0),
    duration_max_ms double precision NOT NULL DEFAULT 0 CHECK (duration_max_ms >= 0),
    rows_returned bigint NOT NULL DEFAULT 0 CHECK (rows_returned >= 0),
    rows_affected bigint NOT NULL DEFAULT 0 CHECK (rows_affected >= 0),
    latency_le_5_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_5_ms >= 0),
    latency_le_10_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_10_ms >= 0),
    latency_le_25_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_25_ms >= 0),
    latency_le_50_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_50_ms >= 0),
    latency_le_100_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_100_ms >= 0),
    latency_le_250_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_250_ms >= 0),
    latency_le_500_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_500_ms >= 0),
    latency_le_1000_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_1000_ms >= 0),
    latency_le_2500_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_2500_ms >= 0),
    latency_le_5000_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_5000_ms >= 0),
    latency_le_15000_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_15000_ms >= 0),
    latency_le_60000_ms bigint NOT NULL DEFAULT 0 CHECK (latency_le_60000_ms >= 0),
    PRIMARY KEY (bucket_start_ms, project_id, fingerprint)
);

CREATE INDEX observability_query_project_time_idx
    ON observability_query_buckets (project_id, bucket_start_ms DESC);
CREATE INDEX observability_query_time_idx
    ON observability_query_buckets (bucket_start_ms DESC);

CREATE TABLE observability_project_storage (
    project_id uuid PRIMARY KEY,
    logical_database_bytes bigint NOT NULL CHECK (logical_database_bytes >= 0),
    sampled_at_ms bigint NOT NULL CHECK (sampled_at_ms >= 0)
);

COMMENT ON TABLE observability_query_buckets IS
    'Privacy-safe aggregate query telemetry. shape contains no SQL identifiers, comments, literal values, or bound parameter values.';
