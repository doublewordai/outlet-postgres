-- Add duration_to_first_byte_ms column to http_responses table
ALTER TABLE http_responses
ADD COLUMN duration_to_first_byte_ms BIGINT;

-- Update existing records to have duration_to_first_byte_ms = duration_ms
-- (for backward compatibility, existing records likely represent first-byte timing)
UPDATE http_responses
SET duration_to_first_byte_ms = duration_ms
WHERE duration_to_first_byte_ms IS NULL;

-- Make the column NOT NULL after populating it
ALTER TABLE http_responses
ALTER COLUMN duration_to_first_byte_ms SET NOT NULL;

-- Add index for new duration field
CREATE INDEX idx_http_responses_duration_to_first_byte_ms ON http_responses(duration_to_first_byte_ms);