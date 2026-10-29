-- Dropping pending changes would leave a stale payload at the newer revision.
-- Clients must upload the full collections first.
CREATE TEMPORARY TABLE encrypted_sync_change_removal_guard (
    row_count BIGINT CONSTRAINT encrypted_sync_changes_must_be_uploaded_in_full CHECK (row_count = 0)
);
INSERT INTO encrypted_sync_change_removal_guard SELECT COUNT(*) FROM encrypted_sync_change;
DROP TABLE encrypted_sync_change_removal_guard;
ALTER TABLE account_session DROP COLUMN reads_collection_changes;
DROP TABLE encrypted_sync_change;
ALTER TABLE encrypted_sync DROP COLUMN base_revision;
