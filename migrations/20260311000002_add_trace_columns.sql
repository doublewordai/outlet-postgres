-- Add OpenTelemetry trace context columns for DB → Tempo correlation
ALTER TABLE http_requests ADD COLUMN trace_id VARCHAR(32);
ALTER TABLE http_requests ADD COLUMN span_id VARCHAR(16);

CREATE INDEX idx_http_requests_trace_id ON http_requests(trace_id) WHERE trace_id IS NOT NULL;
