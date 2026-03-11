-- Convert http_requests and http_responses to partitioned tables.
--
-- This enables time-based retention by allowing old partitions to be dropped.
-- A DEFAULT partition is created so the tables work identically to before
-- without any partition management. To enable retention, create time-range
-- partitions (manually or via pg_partman) and old data can be dropped by
-- simply dropping the partition.
--
-- Example (weekly partitions with pg_partman):
--
--   SELECT partman.create_parent(
--       'public.http_requests', 'timestamp', 'native', 'weekly'
--   );
--   UPDATE partman.part_config
--   SET retention = '30 days', retention_keep_table = false
--   WHERE parent_table = 'public.http_requests';
--
-- Example (manual weekly partitions):
--
--   CREATE TABLE http_requests_2026_w11
--       PARTITION OF http_requests
--       FOR VALUES FROM ('2026-03-09') TO ('2026-03-16');
--
-- Note on uniqueness: PostgreSQL requires the partition key in all unique
-- constraints. The previous UNIQUE (instance_id, correlation_id) is replaced
-- with a non-unique index. Uniqueness is guaranteed by application logic
-- (correlation_id is a monotonic counter per instance_id).

-- ============================================================
-- http_requests
-- ============================================================

-- Rename the existing table and its sequence
ALTER TABLE http_requests RENAME TO http_requests_old;
ALTER SEQUENCE http_requests_id_seq RENAME TO http_requests_old_id_seq;

-- Rename constraints and indexes to avoid conflicts
ALTER TABLE http_requests_old RENAME CONSTRAINT http_requests_pkey TO http_requests_old_pkey;
ALTER TABLE http_requests_old RENAME CONSTRAINT http_requests_instance_correlation_unique TO http_requests_old_instance_correlation_unique;
ALTER INDEX idx_http_requests_timestamp RENAME TO idx_http_requests_old_timestamp;
ALTER INDEX idx_http_requests_method RENAME TO idx_http_requests_old_method;
ALTER INDEX idx_http_requests_instance_correlation RENAME TO idx_http_requests_old_instance_correlation;

-- Create partitioned table (partition key must be in primary key)
CREATE TABLE http_requests (
    id BIGSERIAL NOT NULL,
    instance_id UUID NOT NULL,
    correlation_id BIGINT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL,
    method VARCHAR(10) NOT NULL,
    uri TEXT NOT NULL,
    headers JSONB NOT NULL,
    body JSONB,
    body_parsed BOOLEAN,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, timestamp)
) PARTITION BY RANGE (timestamp);

-- Default partition catches everything when no time-range partitions exist
CREATE TABLE http_requests_default PARTITION OF http_requests DEFAULT;

-- Recreate indexes (instance_id, correlation_id is non-unique — see note above)
CREATE INDEX idx_http_requests_timestamp ON http_requests (timestamp);
CREATE INDEX idx_http_requests_method ON http_requests (method);
CREATE INDEX idx_http_requests_instance_correlation ON http_requests (instance_id, correlation_id);

-- Migrate existing data (explicit columns — old table has instance_id appended at end by ALTER TABLE)
INSERT INTO http_requests (id, instance_id, correlation_id, timestamp, method, uri, headers, body, body_parsed, created_at)
SELECT id, instance_id, correlation_id, timestamp, method, uri, headers, body, body_parsed, created_at
FROM http_requests_old;

-- Reset sequence to continue from where the old table left off
SELECT setval(
    'http_requests_id_seq',
    COALESCE(NULLIF(max_id, 0), 1),
    max_id IS NOT NULL AND max_id > 0
) FROM (SELECT MAX(id) AS max_id FROM http_requests) AS seq;

-- Drop old table (cascades old sequence)
DROP TABLE http_requests_old;

-- ============================================================
-- http_responses
-- ============================================================

-- Rename the existing table and its sequence
ALTER TABLE http_responses RENAME TO http_responses_old;
ALTER SEQUENCE http_responses_id_seq RENAME TO http_responses_old_id_seq;

-- Rename constraints and indexes to avoid conflicts
ALTER TABLE http_responses_old RENAME CONSTRAINT http_responses_pkey TO http_responses_old_pkey;
ALTER TABLE http_responses_old RENAME CONSTRAINT http_responses_instance_correlation_unique TO http_responses_old_instance_correlation_unique;
ALTER INDEX idx_http_responses_timestamp RENAME TO idx_http_responses_old_timestamp;
ALTER INDEX idx_http_responses_status_code RENAME TO idx_http_responses_old_status_code;
ALTER INDEX idx_http_responses_instance_correlation RENAME TO idx_http_responses_old_instance_correlation;
ALTER INDEX idx_http_responses_duration_to_first_byte_ms RENAME TO idx_http_responses_old_duration_to_first_byte_ms;

-- Create partitioned table
CREATE TABLE http_responses (
    id BIGSERIAL NOT NULL,
    instance_id UUID NOT NULL,
    correlation_id BIGINT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL,
    status_code INTEGER NOT NULL,
    headers JSONB NOT NULL,
    body JSONB,
    body_parsed BOOLEAN,
    duration_to_first_byte_ms BIGINT NOT NULL,
    duration_ms BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, timestamp)
) PARTITION BY RANGE (timestamp);

-- Default partition catches everything when no time-range partitions exist
CREATE TABLE http_responses_default PARTITION OF http_responses DEFAULT;

-- Recreate indexes (instance_id, correlation_id is non-unique — see note above)
CREATE INDEX idx_http_responses_timestamp ON http_responses (timestamp);
CREATE INDEX idx_http_responses_status_code ON http_responses (status_code);
CREATE INDEX idx_http_responses_instance_correlation ON http_responses (instance_id, correlation_id);
CREATE INDEX idx_http_responses_duration_to_first_byte_ms ON http_responses (duration_to_first_byte_ms);

-- Migrate existing data (explicit columns — old table has instance_id appended at end by ALTER TABLE)
INSERT INTO http_responses (id, instance_id, correlation_id, timestamp, status_code, headers, body, body_parsed, duration_to_first_byte_ms, duration_ms, created_at)
SELECT id, instance_id, correlation_id, timestamp, status_code, headers, body, body_parsed, duration_to_first_byte_ms, duration_ms, created_at
FROM http_responses_old;

-- Reset sequence to continue from where the old table left off
SELECT setval(
    'http_responses_id_seq',
    COALESCE(NULLIF(max_id, 0), 1),
    max_id IS NOT NULL AND max_id > 0
) FROM (SELECT MAX(id) AS max_id FROM http_responses) AS seq;

-- Drop old table (cascades old sequence)
DROP TABLE http_responses_old;
