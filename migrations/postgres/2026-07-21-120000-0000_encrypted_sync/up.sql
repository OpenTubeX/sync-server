CREATE TABLE encrypted_sync(
    account_id VARCHAR NOT NULL PRIMARY KEY,
    revision BIGINT NOT NULL,
    payload TEXT NOT NULL,
    CONSTRAINT FK__encrypted_sync__account FOREIGN KEY(account_id) REFERENCES account(id) ON DELETE CASCADE
);
