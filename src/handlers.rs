use std::pin::Pin;

use actix_web::{
    FromRequest, HttpMessage, HttpRequest,
    body::MessageBody,
    dev::{ServiceFactory, ServiceRequest, ServiceResponse},
    error::ResponseError,
    http::StatusCode,
};
use utoipa_actix_web::scope::Scope;

use crate::models::Account;

pub mod channel_playback_speeds;
pub mod encrypted_sync;
pub mod health;
pub mod playlist_bookmarks;
pub mod playlists;
pub mod subscriptions;
pub mod user;
pub mod watch_history;

#[derive(thiserror::Error, Debug)]
pub enum HandlerError {
    #[error("bookmark doesn't exists")]
    BookmarkNotExists,
    #[error("playlist doesn't exists")]
    PlaylistNotExists,
    #[error("account doesn't exists")]
    AccountNotExists,
    #[error("not the owner of the playlist")]
    PlaylistNotOwned,
    #[error("playlist already exists")]
    PlaylistExists,
    #[error("not subscribed to this channel")]
    NotSubscribed,
    #[error("subscription group doesn't exist or doesn't belong to this account")]
    SubscriptionGroupNotFound,
    #[error("channel has to be subscribed to before it can be added to a channel group")]
    SubscribeBeforeChannelGroup,
    #[error("registration is disabled on this server")]
    RegistrationDisabled,
    #[error("password too short (8 chars min)")]
    PasswordTooShort,
    #[error("accountname already taken")]
    AccountNameTaken,
    #[error("invalid accountname or password")]
    InvalidCredentials,
    #[error("invalid or missing authentication token")]
    InvalidToken,
    #[error("video not in watch history")]
    NotInWatchHistory,
    #[error("internal database error")]
    InternalDatabaseError,
    #[error("internal database error: {0}")]
    InternalDatabaseErrorWithContext(String),
    #[error("provided metadata seems to be wrong")]
    ValidationError,
    #[error("provided metadata seems to be wrong: {0}")]
    ValidationErrorWithContext(String),
    #[error("encrypted sync collection changed; retry with the latest revision")]
    EncryptedSyncConflict,
    #[error("encrypted sync collection is too large")]
    EncryptedSyncTooLarge,
    #[error("too many items in one request (max {0})")]
    BulkRequestTooLarge(usize),
    #[error("too many requests; slow down and try again later")]
    TooManyRequests,
    #[error("account storage quota exceeded")]
    StorageQuotaExceeded,
    #[error("encrypted sync account storage quota exceeded")]
    EncryptedSyncQuotaExceeded,
    #[error("this account requires the encrypted sync endpoint")]
    EncryptedSyncRequired,
    #[error("failed to load data from YouTube")]
    YouTubeConnectError,
}

impl ResponseError for HandlerError {
    fn status_code(&self) -> actix_web::http::StatusCode {
        match self {
            Self::BookmarkNotExists => StatusCode::NOT_FOUND,
            Self::PlaylistNotExists => StatusCode::NOT_FOUND,
            Self::PlaylistNotOwned => StatusCode::FORBIDDEN,
            Self::PlaylistExists => StatusCode::CONFLICT,
            Self::NotSubscribed => StatusCode::BAD_REQUEST,
            Self::SubscriptionGroupNotFound => StatusCode::NOT_FOUND,
            Self::SubscribeBeforeChannelGroup => StatusCode::BAD_REQUEST,
            Self::RegistrationDisabled => StatusCode::METHOD_NOT_ALLOWED,
            Self::PasswordTooShort => StatusCode::BAD_REQUEST,
            Self::AccountNameTaken => StatusCode::CONFLICT,
            Self::AccountNotExists => StatusCode::NOT_FOUND,
            Self::InvalidCredentials => StatusCode::FORBIDDEN,
            Self::InvalidToken => StatusCode::UNAUTHORIZED,
            Self::NotInWatchHistory => StatusCode::NOT_FOUND,
            Self::InternalDatabaseError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::InternalDatabaseErrorWithContext(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ValidationError => StatusCode::BAD_REQUEST,
            Self::ValidationErrorWithContext(_) => StatusCode::BAD_REQUEST,
            Self::EncryptedSyncConflict => StatusCode::CONFLICT,
            Self::EncryptedSyncTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::BulkRequestTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::StorageQuotaExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::EncryptedSyncQuotaExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::EncryptedSyncRequired => StatusCode::CONFLICT,
            Self::YouTubeConnectError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

pub type HandlerResult<T> = Result<T, HandlerError>;

/// Upper bound on how many items one bulk request may carry.
///
/// Validation can issue a YouTube round-trip per distinct channel, so an
/// unbounded batch lets a single request occupy a worker for an unbounded time.
pub const MAX_BULK_ITEMS: usize = 1000;

pub fn check_bulk_size(len: usize) -> HandlerResult<()> {
    if len > MAX_BULK_ITEMS {
        return Err(HandlerError::BulkRequestTooLarge(MAX_BULK_ITEMS));
    }

    Ok(())
}

/// Reject the request if storing `incoming` more rows would exceed the
/// per-account quota for a plaintext table.
pub fn check_row_quota(stored_rows: i64, incoming: usize) -> HandlerResult<()> {
    if crate::database::quota::exceeds_row_quota(stored_rows, incoming) {
        return Err(HandlerError::StorageQuotaExceeded);
    }

    Ok(())
}

impl From<diesel::result::Error> for HandlerError {
    fn from(error: diesel::result::Error) -> Self {
        Self::InternalDatabaseErrorWithContext(error.to_string())
    }
}

// https://github.com/actix/actix-web/discussions/3074
pub trait ScopedHandler {
    fn get_service() -> Scope<
        impl ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<impl MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        >,
    >;
}

impl FromRequest for Account {
    type Error = actix_web::Error;

    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(req: &HttpRequest, _payload: &mut actix_web::dev::Payload) -> Self::Future {
        let extensions = req.extensions();
        let account = extensions.get::<Account>().cloned();
        Box::pin(
            async move { account.ok_or(actix_web::error::ErrorForbidden("missing account info")) },
        )
    }
}

#[macro_export]
macro_rules! get_db_conn {
    ($pool:ident) => {
        $pool
            .get()
            .await
            .expect("Couldn't get db connection from the pool")
    };
}
