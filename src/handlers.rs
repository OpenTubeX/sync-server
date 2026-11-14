use std::pin::Pin;

use actix_web::{
    FromRequest, HttpMessage, HttpRequest,
    body::MessageBody,
    dev::{ServiceFactory, ServiceRequest, ServiceResponse},
    error::ResponseError,
    http::StatusCode,
};
use utoipa_actix_web::scope::Scope;

use crate::{models::Account, oidc::OidcError};

pub mod encrypted_sync;
pub mod health;
pub mod pairing;
pub mod playlist_bookmarks;
pub mod playlists;
pub mod session;
pub mod subscriptions;
pub mod user;
pub mod watch_history;

#[derive(thiserror::Error, Debug)]
pub enum HandlerError {
    #[error("bookmark doesn't exist")]
    BookmarkNotExists,
    #[error("playlist doesn't exist")]
    PlaylistNotExists,
    #[error("account doesn't exist")]
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
    #[error("password is included in a data breach")]
    PasswordBreached,
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
    #[error("the database is busy; try again shortly")]
    DatabaseBusy,
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
    #[error(
        "another device saved changes to this collection that require a newer OpenTubeX version; update OpenTubeX, or sync again after an updated device has synced new changes"
    )]
    EncryptedSyncChangesPending,
    #[error("upload the full encrypted sync collection")]
    EncryptedSyncFullUploadRequired,
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
    #[error("pairing session not found or expired")]
    PairingNotFound,
    #[error("account session not found or expired")]
    AccountSessionNotFound,
    #[error("pairing session has already changed state")]
    PairingConflict,
    #[error("too many active pairing sessions")]
    PairingLimitExceeded,
    #[error("failed to load data from YouTube")]
    YouTubeConnectError,
    #[error("{0}")]
    OidcError(OidcError),
    #[error("account doesn't support logging in via password")]
    PasswordLoginDisabledForAccount,
    #[error("account doesn't support logging in via OIDC")]
    OidcLoginDisabledForAccount,
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
            Self::PasswordBreached => StatusCode::BAD_REQUEST,
            Self::AccountNameTaken => StatusCode::CONFLICT,
            Self::AccountNotExists => StatusCode::NOT_FOUND,
            Self::InvalidCredentials => StatusCode::FORBIDDEN,
            Self::InvalidToken => StatusCode::UNAUTHORIZED,
            Self::NotInWatchHistory => StatusCode::NOT_FOUND,
            Self::InternalDatabaseError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::DatabaseBusy => StatusCode::SERVICE_UNAVAILABLE,
            Self::InternalDatabaseErrorWithContext(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ValidationError => StatusCode::BAD_REQUEST,
            Self::ValidationErrorWithContext(_) => StatusCode::BAD_REQUEST,
            Self::EncryptedSyncConflict => StatusCode::CONFLICT,
            Self::EncryptedSyncTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::EncryptedSyncChangesPending => StatusCode::CONFLICT,
            Self::EncryptedSyncFullUploadRequired => StatusCode::PRECONDITION_FAILED,
            Self::BulkRequestTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::StorageQuotaExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::EncryptedSyncQuotaExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::EncryptedSyncRequired => StatusCode::CONFLICT,
            Self::PairingNotFound => StatusCode::NOT_FOUND,
            Self::AccountSessionNotFound => StatusCode::NOT_FOUND,
            Self::PairingConflict => StatusCode::CONFLICT,
            Self::PairingLimitExceeded => StatusCode::TOO_MANY_REQUESTS,
            Self::YouTubeConnectError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::OidcError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::PasswordLoginDisabledForAccount => StatusCode::BAD_REQUEST,
            Self::OidcLoginDisabledForAccount => StatusCode::BAD_REQUEST,
        }
    }

    fn error_response(&self) -> actix_web::HttpResponse {
        let mut response = actix_web::HttpResponse::build(self.status_code());
        if matches!(self, Self::DatabaseBusy) {
            response.insert_header((actix_web::http::header::RETRY_AFTER, "30"));
        }
        response
            .insert_header(actix_web::http::header::ContentType::plaintext())
            .body(self.to_string())
    }
}

impl HandlerError {
    /// Log a database error that clients only see as busy or internal.
    pub fn database(error: diesel::result::Error) -> Self {
        if is_database_busy(&error) {
            log::warn!("database busy: {error}");
            Self::DatabaseBusy
        } else {
            log::error!("database error: {error}");
            Self::InternalDatabaseError
        }
    }

    /// Every pooled connection may be waiting for the write lock, so a pool
    /// timeout is as retryable as the busy error those connections will get.
    pub fn pool(error: diesel_async::pooled_connection::bb8::RunError) -> Self {
        match error {
            diesel_async::pooled_connection::bb8::RunError::TimedOut => {
                log::warn!("database busy: no pooled connection became available");
                Self::DatabaseBusy
            }
            diesel_async::pooled_connection::bb8::RunError::User(error) => {
                log::error!("database connection error: {error}");
                Self::InternalDatabaseError
            }
        }
    }
}

/// SQLite reports a write lock it could not get within its busy timeout, and
/// PostgreSQL a transaction that conflicted with a concurrent one.
fn is_database_busy(error: &diesel::result::Error) -> bool {
    use diesel::result::{DatabaseErrorKind, Error};

    match error {
        Error::DatabaseError(DatabaseErrorKind::SerializationFailure, _) => true,
        Error::DatabaseError(_, info) => {
            matches!(
                info.message(),
                "database is locked" | "database table is locked"
            )
        }
        _ => false,
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

/// Fail fast when an account is *already* over its quota.
///
/// Deliberately does not charge for the incoming batch. All the bulk write paths
/// are upserts, so a client re-syncing data it already stored adds no rows;
/// charging the batch up front would reject those re-syncs even though they
/// change nothing, and would do so precisely when an account is near its limit.
/// [`check_stored_rows`] does the authoritative check after the writes.
pub fn check_already_over_quota(stored_rows: i64) -> HandlerResult<()> {
    check_stored_rows(stored_rows)
}

/// Authoritative quota check, for use after the writes inside a transaction.
///
/// Counting the rows that actually exist is exact regardless of how many of the
/// incoming entries were updates rather than inserts. Returning an error from
/// inside the transaction rolls the writes back.
pub fn check_stored_rows(stored_rows: i64) -> HandlerResult<()> {
    if crate::database::quota::exceeds_row_quota(stored_rows) {
        return Err(HandlerError::StorageQuotaExceeded);
    }

    Ok(())
}

impl From<diesel::result::Error> for HandlerError {
    fn from(error: diesel::result::Error) -> Self {
        if is_database_busy(&error) {
            return Self::database(error);
        }
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

impl FromRequest for crate::models::AccountSession {
    type Error = actix_web::Error;

    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(req: &HttpRequest, _payload: &mut actix_web::dev::Payload) -> Self::Future {
        let extensions = req.extensions();
        let session = extensions.get::<crate::models::AccountSession>().cloned();
        Box::pin(async move {
            session.ok_or(actix_web::error::ErrorForbidden(
                "missing account session info",
            ))
        })
    }
}

#[macro_export]
macro_rules! get_db_conn {
    ($pool:ident) => {
        $pool
            .get()
            .await
            .map_err($crate::handlers::HandlerError::pool)?
    };
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use actix_web::{ResponseError, http::StatusCode};
    use diesel_async::SimpleAsyncConnection;

    use super::HandlerError;

    #[actix_rt::test]
    async fn a_held_write_lock_is_reported_as_retryable() {
        let (database, mut writer) = crate::database::TestFileDatabase::new().await;
        writer.batch_execute("BEGIN IMMEDIATE;").await.unwrap();
        let mut blocked = database.connect_without_waiting().await;
        let error = blocked
            .batch_execute("DELETE FROM account;")
            .await
            .unwrap_err();

        let response = HandlerError::database(error).error_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            response
                .headers()
                .contains_key(actix_web::http::header::RETRY_AFTER)
        );
    }

    /// Requests that cannot get a connection, because every pooled one waits
    /// for the write lock, get a retryable response instead of a dropped
    /// connection.
    #[actix_rt::test]
    async fn an_exhausted_pool_is_reported_as_retryable() {
        use actix_web::{App, HttpResponse, get, test, web};
        use diesel_async::pooled_connection::{AsyncDieselConnectionManager, bb8::Pool};

        #[get("/")]
        async fn handler(pool: crate::WebData) -> super::HandlerResult<HttpResponse> {
            let _conn = crate::get_db_conn!(pool);
            Ok(HttpResponse::Ok().finish())
        }

        let pool = Pool::builder()
            .max_size(1)
            .connection_timeout(std::time::Duration::from_millis(10))
            .build(AsyncDieselConnectionManager::<crate::DbConnection>::new(
                ":memory:",
            ))
            .await
            .unwrap();
        let _held = pool.get_owned().await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(pool.clone()))
                .service(handler),
        )
        .await;
        let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            response
                .headers()
                .contains_key(actix_web::http::header::RETRY_AFTER)
        );
    }
}
