use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::database::{account_session, pairing};
use crate::handlers::{HandlerError, HandlerResult};

pub const DEVICE_ID_BYTES: usize = 16;

const MAX_DEVICE_NAME_CHARS: usize = 80;
const MAX_DEVICE_NAME_BYTES: usize = 240;

#[derive(Clone, Copy)]
enum SessionKind {
    Account,
    Pairing,
}

impl SessionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Pairing => "pairing",
        }
    }
}

pub fn start_expired_session_cleanup(pool: crate::DbPool) {
    spawn_cleanup(
        pool.clone(),
        Duration::from_secs(60 * 60),
        SessionKind::Account,
    );
    spawn_cleanup(pool, Duration::from_secs(30), SessionKind::Pairing);
}

fn spawn_cleanup(pool: crate::DbPool, interval: Duration, kind: SessionKind) {
    actix_web::rt::spawn(async move {
        let mut interval = actix_web::rt::time::interval(interval);
        loop {
            interval.tick().await;
            let Ok(now) = now_ms() else {
                log::error!(
                    "could not determine the time for {}-session cleanup",
                    kind.label()
                );
                continue;
            };
            let Ok(mut conn) = pool.get().await else {
                log::error!(
                    "could not get a database connection for {}-session cleanup",
                    kind.label()
                );
                continue;
            };
            let result = match kind {
                SessionKind::Account => account_session::delete_expired(&mut conn, now).await,
                SessionKind::Pairing => pairing::delete_expired(&mut conn, now).await,
            };
            if let Err(error) = result {
                log::error!(
                    "could not delete expired {} sessions: {error}",
                    kind.label()
                );
            }
        }
    });
}

pub fn now_ms() -> HandlerResult<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HandlerError::InternalDatabaseError)?
        .as_millis();
    i64::try_from(millis).map_err(|_| HandlerError::InternalDatabaseError)
}

pub fn is_base64url(value: &str, expected_length: usize) -> bool {
    URL_SAFE_NO_PAD
        .decode(value)
        .is_ok_and(|bytes| bytes.len() == expected_length && URL_SAFE_NO_PAD.encode(bytes) == value)
}

pub fn validate_device_id(value: &str) -> HandlerResult<()> {
    if !is_base64url(value, DEVICE_ID_BYTES) {
        return Err(HandlerError::ValidationErrorWithContext(
            "invalid device id".to_owned(),
        ));
    }
    Ok(())
}

pub fn validate_device_name(value: &str) -> HandlerResult<()> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > MAX_DEVICE_NAME_CHARS
        || value.len() > MAX_DEVICE_NAME_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(HandlerError::ValidationErrorWithContext(
            "invalid device name".to_owned(),
        ));
    }
    Ok(())
}
