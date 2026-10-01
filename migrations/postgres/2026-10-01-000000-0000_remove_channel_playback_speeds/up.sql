-- Existing plaintext speeds must be migrated before retiring their table.
LOCK TABLE channel_playback_speed IN ACCESS EXCLUSIVE MODE;
CREATE TEMPORARY TABLE playback_speed_removal_guard (
    row_count BIGINT CONSTRAINT plaintext_playback_speeds_must_be_migrated CHECK (row_count = 0)
);
INSERT INTO playback_speed_removal_guard SELECT COUNT(*) FROM channel_playback_speed;
DROP TABLE channel_playback_speed;
DROP TABLE playback_speed_removal_guard;
