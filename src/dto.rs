use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::models::{
    Channel, Playlist, PublicPlaylist, SubscriptionGroup, Video, WatchHistoryItem,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RegisterUser {
    pub name: String,
    pub password: String,
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct LoginUser {
    pub name: String,
    pub password: String,
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct LoginResponse {
    pub jwt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SyncCapabilities {
    pub encrypted_sync: u8,
    pub bulk_sync: u8,
    pub history_page_size: u32,
    pub key_pairing: u8,
    pub account_sessions: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub capabilities: SyncCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EncryptedSyncManifest {
    pub collections: Vec<EncryptedSyncCollectionRevision>,
    pub legacy_data: bool,
    pub legacy_encrypted_data: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EncryptedSyncCollectionRevision {
    pub collection: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EncryptedSyncCollectionResponse {
    pub collection: String,
    pub revision: i64,
    pub payload: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PutEncryptedSync {
    pub revision: i64,
    pub payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreatePairingSession {
    pub version: u8,
    pub id: String,
    pub recipient_public_key: String,
    pub recipient_device_id: String,
    pub recipient_device_name: String,
    pub recipient_token_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimPairingSession {
    pub version: u8,
    pub recipient_public_key: String,
    pub recipient_device_id: String,
    pub recipient_device_name: String,
    pub encrypted_device_info: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovePairingSession {
    pub approving_device_id: String,
    pub encrypted_payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PairingSessionResponse {
    pub version: u8,
    pub id: String,
    pub account_id: Option<String>,
    pub recipient_public_key: String,
    pub recipient_device_id: String,
    pub recipient_device_name: String,
    pub approving_device_id: Option<String>,
    pub expires_at: i64,
    pub approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PairingClaimResponse {
    pub session: PairingSessionResponse,
    pub jwt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PairingPayloadResponse {
    pub approving_device_id: String,
    pub encrypted_payload: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct DeleteUser {
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAccountSession {
    pub encrypted_device_info: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountSessionResponse {
    pub id: String,
    pub device_id: String,
    pub encrypted_device_info: Option<String>,
    pub created_at: i64,
    pub last_active_at: i64,
    pub expires_at: i64,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountSessionsResponse {
    pub sessions: Vec<AccountSessionResponse>,
    pub password_login: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CreatePlaylist {
    pub id: Option<String>,
    pub title: String,
    pub description: String,
    pub thumbnail_url: Option<String>,
}

/// Public (API) view of a playlist owned by a user.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Eq, PartialEq)]
pub struct ExtendedPlaylist {
    pub id: String,
    pub title: String,
    pub description: String,
    pub thumbnail_url: Option<String>,
    // only difference from playlist is this video count field:
    // ugly workaround because of https://github.com/diesel-rs/diesel/issues/860
    pub video_count: Option<u64>,
}
impl ExtendedPlaylist {
    pub fn from_playlist(playlist: &Playlist, video_count: u64) -> Self {
        ExtendedPlaylist {
            id: playlist.id.clone(),
            title: playlist.title.clone(),
            description: playlist.description.clone(),
            thumbnail_url: playlist.thumbnail_url.clone(),
            video_count: Some(video_count),
        }
    }

    pub fn from_public_playlist(playlist: &PublicPlaylist) -> Self {
        ExtendedPlaylist {
            id: playlist.id.clone(),
            title: playlist.title.clone(),
            description: playlist.description.clone(),
            thumbnail_url: playlist.thumbnail_url.clone(),
            video_count: playlist.video_count.map(|count| count as u64),
        }
    }
}
impl ExtendedPlaylist {
    pub fn into_public_playlist(self, uploader_id: &str) -> PublicPlaylist {
        PublicPlaylist {
            id: self.id,
            title: self.title,
            description: self.description,
            thumbnail_url: self.thumbnail_url,
            video_count: self.video_count.map(|count| count as i32),
            uploader_id: uploader_id.to_string(),
        }
    }
}

/// Public (API) view of a read-only playlist (e.g. from YouTube).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Eq, PartialEq)]
pub struct ExtendedPublicPlaylist {
    pub playlist: ExtendedPlaylist,
    pub uploader: Channel,
}
impl ExtendedPublicPlaylist {
    pub fn from_public_playlist(playlist: &PublicPlaylist, channel: &Channel) -> Self {
        ExtendedPublicPlaylist {
            playlist: ExtendedPlaylist {
                id: playlist.id.clone(),
                title: playlist.title.clone(),
                description: playlist.description.clone(),
                thumbnail_url: playlist.thumbnail_url.clone(),
                video_count: playlist.video_count.map(|c| c as u64),
            },
            uploader: channel.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistResponse {
    pub playlist: ExtendedPlaylist,
    pub videos: Vec<CreateVideo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateVideo {
    pub id: String,
    pub title: String,
    /// Upload date as UNIX timestamp (millis).
    pub upload_date: i64,
    pub uploader: Channel,
    pub thumbnail_url: String,
    pub duration: i32,
}
impl From<(&Video, &Channel)> for CreateVideo {
    fn from((video, channel): (&Video, &Channel)) -> Self {
        CreateVideo {
            id: video.id.clone(),
            title: video.title.clone(),
            upload_date: video.upload_date,
            thumbnail_url: video.thumbnail_url.clone(),
            duration: video.duration,
            uploader: channel.clone(),
        }
    }
}
impl From<&CreateVideo> for Video {
    fn from(val: &CreateVideo) -> Self {
        Video {
            id: val.id.clone(),
            title: val.title.clone(),
            upload_date: val.upload_date,
            uploader_id: val.uploader.id.clone(),
            thumbnail_url: val.thumbnail_url.clone(),
            duration: val.duration,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExtendedWatchHistoryItem {
    pub video: CreateVideo,
    pub metadata: WatchHistoryItem,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExtendedSubscriptionGroup {
    pub group: SubscriptionGroup,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, ToSchema)]
pub enum WatchedState {
    #[serde(rename = "planned")]
    Planned,
    #[serde(rename = "watching")]
    Watching,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "dropped")]
    Dropped,
}

/// Claims to store inside the JWT Token
#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
    /// User ID.
    pub sub: String,
    /// Database-backed account session ID.
    #[serde(default)]
    pub jti: Option<String>,
    pub exp: usize,
}
