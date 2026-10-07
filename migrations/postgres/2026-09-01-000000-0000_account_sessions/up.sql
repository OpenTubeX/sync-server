ALTER TABLE account ADD COLUMN legacy_tokens_enabled BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE account ADD COLUMN session_generation BIGINT NOT NULL DEFAULT 0;

CREATE TABLE account_session(
    id VARCHAR PRIMARY KEY NOT NULL,
    account_id VARCHAR NOT NULL,
    device_id VARCHAR NOT NULL,
    encrypted_device_info TEXT,
    created_at BIGINT NOT NULL,
    last_active_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    revoked_at BIGINT,
    legacy BOOLEAN NOT NULL DEFAULT FALSE,
    generation BIGINT NOT NULL,
    pending_pairing BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT FK__account_session__account FOREIGN KEY(account_id) REFERENCES account(id) ON DELETE CASCADE
);

CREATE INDEX account_session_account_expires_idx
    ON account_session(account_id, expires_at);
