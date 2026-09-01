pub mod account;
pub mod account_session;
pub mod channel;
pub mod channel_playback_speed;
pub mod encrypted_sync;
pub mod pairing;
pub mod playlist;
pub mod playlist_bookmark;
pub mod public_playlist;
pub mod quota;
pub mod subscription;
pub mod subscription_groups;
pub mod video;
pub mod watch_history;

type DbError = diesel::result::Error;
