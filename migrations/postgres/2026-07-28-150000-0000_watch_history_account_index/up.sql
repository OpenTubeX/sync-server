-- watch_history is keyed by (video_id, account_id), so account_id is not the
-- leading column and per-account lookups cannot use the primary key index.
-- The row quota counts rows per account on every write, and clearing or
-- migrating an account's history also filters on it alone.
CREATE INDEX IF NOT EXISTS watch_history_account_id ON watch_history (account_id);
