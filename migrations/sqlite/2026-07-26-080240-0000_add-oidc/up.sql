-- Rebuilding the parent table while foreign keys are enabled would execute the
-- ON DELETE CASCADE actions and erase every account's sync data. This migration
-- runs outside a transaction so these PRAGMAs take effect.
PRAGMA foreign_keys = OFF;

-- Make password_hash nullable.
CREATE TABLE account_temp(
    id VARCHAR PRIMARY KEY NOT NULL,
    name_hash VARCHAR NOT NULL UNIQUE,
    password_hash VARCHAR NULL
);
INSERT INTO account_temp SELECT * FROM account;
DROP TABLE account;
ALTER TABLE account_temp RENAME TO account;

-- add oidc sub
ALTER TABLE account ADD oidc_sub VARCHAR NULL DEFAULT NULL;

PRAGMA foreign_keys = ON;
