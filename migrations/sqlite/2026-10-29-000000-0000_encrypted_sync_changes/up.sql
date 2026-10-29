-- A full upload sets `base_revision` to its revision. Appended changes advance
-- `revision` and are stored separately until the next full upload. Existing
-- collections keep 0, which means their base is their revision: filling the
-- column would rewrite every row with its payload.
ALTER TABLE encrypted_sync ADD COLUMN base_revision BIGINT NOT NULL DEFAULT 0;

CREATE TABLE encrypted_sync_change(
    account_id VARCHAR NOT NULL,
    collection VARCHAR NOT NULL,
    revision BIGINT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY(account_id, collection, revision),
    CONSTRAINT FK__encrypted_sync_change__encrypted_sync FOREIGN KEY(account_id, collection)
        REFERENCES encrypted_sync(account_id, collection) ON DELETE CASCADE
);

-- Clients that only read full collections would miss appended changes, so
-- their sessions block appends.
ALTER TABLE account_session ADD COLUMN reads_collection_changes BOOLEAN NOT NULL DEFAULT FALSE;
