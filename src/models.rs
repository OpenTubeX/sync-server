use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::schema::*;

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    AsChangeset,
    ToSchema,
    Eq,
    PartialEq,
)]
#[diesel(table_name = account)]
pub struct Account {
    pub id: String,
    // Never serialize credential material into a response body. Deserialization
    // is kept so the struct still round-trips through diesel.
    #[serde(skip_serializing)]
    pub name_hash: String,
    #[serde(skip_serializing)]
    pub password_hash: Option<String>,
    #[serde(skip_serializing)]
    pub oidc_sub: Option<String>,
    #[serde(skip_serializing)]
    pub legacy_tokens_enabled: bool,
    #[serde(skip_serializing)]
    pub session_generation: i64,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    AsChangeset,
    ToSchema,
    Eq,
    PartialEq,
)]
#[diesel(belongs_to(Account))]
#[diesel(table_name = account_session)]
pub struct AccountSession {
    pub id: String,
    pub account_id: String,
    pub device_id: String,
    pub encrypted_device_info: Option<String>,
    pub created_at: i64,
    pub last_active_at: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    pub legacy: bool,
    pub generation: i64,
    pub pending_pairing: bool,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    AsChangeset,
    ToSchema,
    Eq,
    PartialEq,
    Hash,
)]
#[diesel(table_name = channel)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub verified: bool,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable, ToSchema, Eq, PartialEq,
)]
#[diesel(primary_key(account_id, channel_id))]
#[diesel(belongs_to(Account))]
#[diesel(belongs_to(Channel))]
#[diesel(table_name = subscription)]
pub struct Subscription {
    #[serde(skip)]
    pub account_id: String,
    pub channel_id: String,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    AsChangeset,
    ToSchema,
    PartialEq,
)]
#[diesel(primary_key(account_id, channel_id))]
#[diesel(belongs_to(Account))]
#[diesel(table_name = channel_playback_speed)]
pub struct ChannelPlaybackSpeed {
    #[serde(skip)]
    pub account_id: String,
    pub channel_id: String,
    pub playback_speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable, ToSchema)]
#[diesel(primary_key(account_id, collection))]
#[diesel(belongs_to(Account))]
#[diesel(table_name = encrypted_sync)]
pub struct EncryptedSync {
    pub account_id: String,
    pub collection: String,
    pub revision: i64,
    pub payload: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, Eq, PartialEq)]
#[diesel(belongs_to(Account))]
#[diesel(table_name = pairing_session)]
pub struct PairingSession {
    pub id: String,
    pub version: i16,
    pub account_id: Option<String>,
    pub recipient_public_key: String,
    pub recipient_device_id: String,
    pub recipient_device_name: String,
    pub recipient_token_hash: String,
    pub approving_device_id: Option<String>,
    pub encrypted_payload: Option<String>,
    pub expires_at: i64,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    AsChangeset,
    ToSchema,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
)]
#[diesel(belongs_to(Account))]
#[diesel(table_name = subscription_group)]
pub struct SubscriptionGroup {
    pub id: String,
    #[serde(skip)]
    pub account_id: String,
    pub title: String,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable, ToSchema, Eq, PartialEq,
)]
#[diesel(primary_key(channel_group_id, channel_id))]
#[diesel(belongs_to(ChannelGroup))]
#[diesel(belongs_to(Channel))]
#[diesel(table_name = subscription_group_member)]
pub struct SubscriptionGroupMember {
    pub subscription_group_id: String,
    pub channel_id: String,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    AsChangeset,
    ToSchema,
    Eq,
    PartialEq,
)]
#[diesel(belongs_to(Account))]
#[diesel(table_name = playlist)]
pub struct Playlist {
    pub id: String,
    #[serde(skip)]
    pub account_id: String,
    pub title: String,
    pub description: String,
    pub thumbnail_url: Option<String>,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    AsChangeset,
    ToSchema,
    Eq,
    PartialEq,
)]
#[diesel(belongs_to(Channel, foreign_key = uploader_id))]
#[diesel(table_name = video)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub upload_date: i64,
    /// ID of the uploader.
    pub uploader_id: String,
    pub thumbnail_url: String,
    /// Duration in seconds.
    pub duration: i32,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable, ToSchema, Eq, PartialEq,
)]
#[diesel(primary_key(account_id, playlist_id, video_id))]
#[diesel(belongs_to(Account))]
#[diesel(belongs_to(Playlist))]
#[diesel(belongs_to(Video))]
#[diesel(table_name = playlist_video_member)]
pub struct PlaylistVideoMember {
    pub account_id: String,
    pub playlist_id: String,
    pub video_id: String,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    AsChangeset,
    ToSchema,
    Eq,
    PartialEq,
)]
#[diesel(table_name = public_playlist)]
#[diesel(belongs_to(Channel, foreign_key = uploader_id))]
pub struct PublicPlaylist {
    pub id: String,
    pub title: String,
    pub description: String,
    pub thumbnail_url: Option<String>,
    pub uploader_id: String,
    pub video_count: Option<i32>,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable, ToSchema, Eq, PartialEq,
)]
#[diesel(primary_key(account_id, public_playlist_id))]
#[diesel(belongs_to(PublicPlaylist))]
#[diesel(belongs_to(Account))]
#[diesel(table_name = playlist_bookmark)]
pub struct PlaylistBookmark {
    #[serde(skip)]
    pub account_id: String,
    pub public_playlist_id: String,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Queryable,
    Selectable,
    Insertable,
    ToSchema,
    Eq,
    PartialEq,
    AsChangeset,
)]
#[diesel(primary_key(account_id, video_id))]
#[diesel(belongs_to(Video))]
#[diesel(belongs_to(Account))]
#[diesel(table_name = watch_history)]
pub struct WatchHistoryItem {
    #[serde(skip)]
    pub video_id: String,
    #[serde(skip)]
    pub account_id: String,
    /// Date as UNIX timestamp (millis).
    pub added_date: i64,
    /// See the available options in [WatchedState].
    pub watched_state: String,
    pub position_millis: Option<i32>,
}
