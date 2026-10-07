DROP TABLE account_session;
ALTER TABLE account DROP COLUMN legacy_tokens_enabled;
ALTER TABLE account DROP COLUMN session_generation;
