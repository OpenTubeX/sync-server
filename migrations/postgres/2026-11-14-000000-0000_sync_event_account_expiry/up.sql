-- Activity events are filtered by account, recipient and expiry, often only
-- for their IDs. Index those columns so these queries can skip the rows and
-- their large padded payloads.
CREATE INDEX sync_event_account_expiry ON sync_event(account_id, recipient, expires_at, id);
