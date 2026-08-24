ALTER TABLE encrypted_sync RENAME TO encrypted_sync_single_document;

CREATE TABLE encrypted_sync(
    account_id VARCHAR NOT NULL,
    collection VARCHAR NOT NULL,
    revision BIGINT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY(account_id, collection),
    CONSTRAINT FK__encrypted_sync__account FOREIGN KEY(account_id) REFERENCES account(id) ON DELETE CASCADE
);
