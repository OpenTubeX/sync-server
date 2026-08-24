CREATE TABLE channel_playback_speed(
	account_id VARCHAR NOT NULL,
	channel_id VARCHAR NOT NULL,
	playback_speed DOUBLE PRECISION NOT NULL,
	PRIMARY KEY(account_id, channel_id),
	CONSTRAINT FK__channel_playback_speed__account FOREIGN KEY(account_id) REFERENCES account(id) ON DELETE CASCADE
);
