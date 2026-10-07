PRAGMA foreign_keys = OFF;
BEGIN;

CREATE TABLE channel_temp
(
    id VARCHAR(24) PRIMARY KEY NOT NULL,
    name VARCHAR NOT NULL,
    avatar VARCHAR NOT NULL,
    verified BOOLEAN NOT NULL
);
INSERT INTO channel_temp
SELECT id, name, COALESCE(avatar, ''), verified FROM channel;
DROP TABLE channel;
ALTER TABLE channel_temp RENAME TO channel;

COMMIT;
PRAGMA foreign_keys = ON;
