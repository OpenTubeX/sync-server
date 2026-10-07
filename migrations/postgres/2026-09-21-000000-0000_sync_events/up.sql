CREATE TABLE sync_event (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    recipient TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE INDEX sync_event_account_recipient ON sync_event(account_id, recipient, created_at);
CREATE INDEX sync_event_expiry ON sync_event(expires_at);
