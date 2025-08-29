-- Migration to add instance_id for correlation_id uniqueness across application restarts
-- 
-- MANUAL OPERATION REQUIRED BEFORE RUNNING THIS MIGRATION:
-- If you have existing data in http_requests/http_responses tables, you must manually
-- clean it up to avoid constraint violations from duplicate correlation_ids:
--
--   TRUNCATE TABLE http_responses;
--   TRUNCATE TABLE http_requests;
--
-- This is required because correlation_ids were generated starting from 0 on each
-- application restart, causing duplicates across different runs.

-- Add instance_id column to both tables with temporary default
ALTER TABLE http_requests ADD COLUMN instance_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
ALTER TABLE http_responses ADD COLUMN instance_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

-- Remove default after adding column (new rows will require explicit instance_id)
ALTER TABLE http_requests ALTER COLUMN instance_id DROP DEFAULT;
ALTER TABLE http_responses ALTER COLUMN instance_id DROP DEFAULT;

-- Add unique constraints on the composite key (instance_id, correlation_id)
ALTER TABLE http_requests ADD CONSTRAINT http_requests_instance_correlation_unique UNIQUE (instance_id, correlation_id);
ALTER TABLE http_responses ADD CONSTRAINT http_responses_instance_correlation_unique UNIQUE (instance_id, correlation_id);

-- Update existing correlation_id indexes to composite indexes for better join performance
DROP INDEX idx_http_requests_correlation_id;
DROP INDEX idx_http_responses_correlation_id;

-- Create composite indexes for correlation joins
CREATE INDEX idx_http_requests_instance_correlation ON http_requests(instance_id, correlation_id);
CREATE INDEX idx_http_responses_instance_correlation ON http_responses(instance_id, correlation_id);