ALTER TABLE account_session DROP COLUMN reads_collection_changes;
DROP TABLE encrypted_sync_change;
ALTER TABLE encrypted_sync DROP COLUMN base_revision;
