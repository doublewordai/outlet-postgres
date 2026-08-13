-- Serialize subject-attributed capture against erasure and prevent late
-- request/response writes from recreating data after an erasure completes.
-- Only a one-way fingerprint is retained; the subject value is never stored
-- in this coordination table.
CREATE TABLE subject_capture_state (
    subject_fingerprint BYTEA PRIMARY KEY,
    erased_at TIMESTAMPTZ NOT NULL
);

COMMENT ON TABLE subject_capture_state IS
    'One-way subject fingerprints used to serialize capture with erasure and reject late writes.';

CREATE INDEX subject_capture_state_erased_at_idx
    ON subject_capture_state (erased_at)
    WHERE erased_at IS NOT NULL;
