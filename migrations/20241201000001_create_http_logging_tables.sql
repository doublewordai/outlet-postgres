-- Create tables for logging HTTP requests and responses
CREATE TABLE http_requests (
    id BIGSERIAL PRIMARY KEY,
    correlation_id BIGINT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL,
    method VARCHAR(10) NOT NULL,
    uri TEXT NOT NULL,
    headers JSONB NOT NULL,
    body JSONB,
    body_parsed BOOLEAN,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE http_responses (
    id BIGSERIAL PRIMARY KEY,
    correlation_id BIGINT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL,
    status_code INTEGER NOT NULL,
    headers JSONB NOT NULL,
    body JSONB,
    body_parsed BOOLEAN,
    duration_ms BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Create indexes for performance
CREATE INDEX idx_http_requests_correlation_id ON http_requests(correlation_id);
CREATE INDEX idx_http_requests_timestamp ON http_requests(timestamp);
CREATE INDEX idx_http_requests_method ON http_requests(method);

CREATE INDEX idx_http_responses_correlation_id ON http_responses(correlation_id);
CREATE INDEX idx_http_responses_timestamp ON http_responses(timestamp);
CREATE INDEX idx_http_responses_status_code ON http_responses(status_code);
