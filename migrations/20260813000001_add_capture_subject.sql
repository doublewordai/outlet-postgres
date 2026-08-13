-- Opaque application subject used for targeted lifecycle operations.
-- Indexes are intentionally left to operators because existing table sizes and
-- online index requirements vary between deployments.
ALTER TABLE http_requests ADD COLUMN subject_id TEXT;
ALTER TABLE http_responses ADD COLUMN subject_id TEXT;
