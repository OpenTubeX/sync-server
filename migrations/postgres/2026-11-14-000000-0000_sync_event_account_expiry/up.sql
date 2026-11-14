-- Activity payloads are padded to tens of kilobytes. SQLite stores most of
-- such a row in overflow pages, with the columns after `payload` at the end,
-- so filtering events by `expires_at` read every stored payload, inside each
-- write transaction with activity and on every change poll. Index the columns
-- these queries filter and return instead.
CREATE INDEX sync_event_account_expiry ON sync_event(account_id, recipient, expires_at, id);
