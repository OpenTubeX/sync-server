CREATE TABLE pairing_session(
    id VARCHAR PRIMARY KEY NOT NULL,
    version SMALLINT NOT NULL,
    account_id VARCHAR,
    recipient_public_key VARCHAR NOT NULL,
    recipient_device_id VARCHAR NOT NULL,
    recipient_device_name VARCHAR NOT NULL,
    recipient_token_hash VARCHAR NOT NULL,
    approving_device_id VARCHAR,
    encrypted_payload TEXT,
    expires_at BIGINT NOT NULL,
    CONSTRAINT FK__pairing_session__account FOREIGN KEY(account_id) REFERENCES account(id) ON DELETE CASCADE
);

CREATE INDEX pairing_session_account_expires_idx
    ON pairing_session(account_id, expires_at);
