-- Rebuilding the parent table while foreign keys are enabled would execute the
-- ON DELETE actions and erase rows that reference channels. This migration runs
-- outside Diesel's transaction so the first PRAGMA takes effect, then wraps the
-- rebuild in its own transaction.
PRAGMA foreign_keys = OFF;
BEGIN;

-- Make avatar nullable.
CREATE TABLE channel_temp
(
    id VARCHAR(24) PRIMARY KEY NOT NULL,
    name VARCHAR NOT NULL,
    avatar VARCHAR NULL,
    verified BOOLEAN NOT NULL
);
INSERT INTO channel_temp SELECT * FROM channel;
DROP TABLE channel;
ALTER TABLE channel_temp RENAME TO channel;

COMMIT;
PRAGMA foreign_keys = ON;
