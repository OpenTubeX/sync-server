use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::{HttpRequest, HttpResponse, Responder, delete, get, post, put, web};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use utoipa_actix_web::scope;

use crate::database::pairing;
use crate::dto::{
    ApprovePairingSession, ClaimPairingSession, CreatePairingSession, PairingClaimResponse,
    PairingPayloadResponse, PairingSessionResponse,
};
use crate::handlers::session::{is_base64url, now_ms, validate_device_id, validate_device_name};
use crate::handlers::user::{
    authenticate_account, new_pairing_account_session, request_within_rate_limit, session_token,
    validate_encrypted_device_info,
};
use crate::handlers::{HandlerError, HandlerResult, ScopedHandler};
use crate::models::{Account, PairingSession};
use crate::rate_limit::RateLimiter;
use crate::{WebData, get_db_conn};

const PAIRING_TTL_MS: i64 = 2 * 60 * 1000;
const PAIRING_PROTOCOL_VERSION: u8 = 1;
const SESSION_ID_BYTES: usize = 32;
const PUBLIC_KEY_BYTES: usize = 32;
const RECIPIENT_TOKEN_BYTES: usize = 32;
const RECIPIENT_TOKEN_HEADER: &str = "X-Pairing-Token";
const MIN_ENCRYPTED_PAYLOAD_BYTES: usize = 96;
const MAX_ENCRYPTED_PAYLOAD_BYTES: usize = 1536;
const MAX_ENCRYPTED_PAYLOAD_LENGTH: usize = 2048;
const MAX_PAIRING_REQUESTS_PER_MINUTE: u32 = 120;
const MAX_TRACKED_ACCOUNTS: usize = 100_000;

struct PairingRateWindow {
    started_at: Instant,
    count: u32,
}

struct PairingRateLimiter {
    windows: Mutex<HashMap<String, PairingRateWindow>>,
}

impl PairingRateLimiter {
    fn check(&self, owner_id: &str) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("pairing rate limiter poisoned");
        if windows.len() >= MAX_TRACKED_ACCOUNTS && !windows.contains_key(owner_id) {
            windows.retain(|_, window| {
                now.duration_since(window.started_at) < Duration::from_secs(60)
            });
            if windows.len() >= MAX_TRACKED_ACCOUNTS {
                return false;
            }
        }
        let window = windows
            .entry(owner_id.to_owned())
            .or_insert(PairingRateWindow {
                started_at: now,
                count: 0,
            });
        if now.duration_since(window.started_at) >= Duration::from_secs(60) {
            window.started_at = now;
            window.count = 0;
        }
        window.count += 1;
        window.count <= MAX_PAIRING_REQUESTS_PER_MINUTE
    }
}

static PAIRING_RATE_LIMITER: LazyLock<PairingRateLimiter> = LazyLock::new(|| PairingRateLimiter {
    windows: Mutex::new(HashMap::new()),
});

pub struct PairingHandler {}

impl ScopedHandler for PairingHandler {
    fn get_service() -> scope::Scope<
        impl ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<impl MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        >,
    > {
        scope::scope("/pairing")
            .app_data(web::JsonConfig::default().limit(MAX_ENCRYPTED_PAYLOAD_LENGTH + 1024))
            .service(create_pairing_session)
            .service(get_pairing_session)
            .service(claim_pairing_session)
            .service(approve_pairing_session)
            .service(consume_pairing_session)
            .service(cancel_pairing_session)
    }
}

fn check_account_rate_limit(account: &Account) -> HandlerResult<()> {
    if !PAIRING_RATE_LIMITER.check(&account.id) {
        return Err(HandlerError::TooManyRequests);
    }
    Ok(())
}

fn validate_session_id(value: &str) -> HandlerResult<()> {
    if !is_base64url(value, SESSION_ID_BYTES) {
        return Err(HandlerError::ValidationErrorWithContext(
            "invalid pairing session id".to_owned(),
        ));
    }
    Ok(())
}

fn validate_pairing_fields(
    version: u8,
    recipient_public_key: &str,
    recipient_device_id: &str,
    recipient_device_name: &str,
) -> HandlerResult<()> {
    if version != PAIRING_PROTOCOL_VERSION {
        return Err(HandlerError::ValidationErrorWithContext(
            "unsupported pairing protocol version".to_owned(),
        ));
    }
    if !is_base64url(recipient_public_key, PUBLIC_KEY_BYTES) {
        return Err(HandlerError::ValidationErrorWithContext(
            "invalid pairing recipient public key".to_owned(),
        ));
    }
    validate_device_id(recipient_device_id)?;
    validate_device_name(recipient_device_name)
}

fn validate_create(form: &CreatePairingSession) -> HandlerResult<()> {
    validate_session_id(&form.id)?;
    validate_pairing_fields(
        form.version,
        &form.recipient_public_key,
        &form.recipient_device_id,
        &form.recipient_device_name,
    )?;
    if !is_base64url(&form.recipient_token_hash, RECIPIENT_TOKEN_BYTES) {
        return Err(HandlerError::ValidationErrorWithContext(
            "invalid pairing recipient token hash".to_owned(),
        ));
    }
    Ok(())
}

fn validate_claim(form: &ClaimPairingSession) -> HandlerResult<()> {
    validate_pairing_fields(
        form.version,
        &form.recipient_public_key,
        &form.recipient_device_id,
        &form.recipient_device_name,
    )?;
    if let Some(encrypted_device_info) = &form.encrypted_device_info {
        validate_encrypted_device_info(encrypted_device_info)?;
    }
    Ok(())
}

fn validate_approval(form: &ApprovePairingSession) -> HandlerResult<()> {
    validate_device_id(&form.approving_device_id)?;
    let valid_payload = URL_SAFE_NO_PAD
        .decode(&form.encrypted_payload)
        .is_ok_and(|bytes| {
            (MIN_ENCRYPTED_PAYLOAD_BYTES..=MAX_ENCRYPTED_PAYLOAD_BYTES).contains(&bytes.len())
                && URL_SAFE_NO_PAD.encode(bytes) == form.encrypted_payload
        });
    if !valid_payload {
        return Err(HandlerError::ValidationErrorWithContext(
            "invalid encrypted pairing payload".to_owned(),
        ));
    }
    Ok(())
}

fn recipient_token_hash(request: &HttpRequest) -> HandlerResult<String> {
    let token = request
        .headers()
        .get(RECIPIENT_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(HandlerError::InvalidToken)?;
    let token = URL_SAFE_NO_PAD
        .decode(token)
        .ok()
        .filter(|bytes| {
            bytes.len() == RECIPIENT_TOKEN_BYTES && URL_SAFE_NO_PAD.encode(bytes) == token
        })
        .ok_or(HandlerError::InvalidToken)?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(token)))
}

fn response(session: PairingSession) -> PairingSessionResponse {
    PairingSessionResponse {
        version: PAIRING_PROTOCOL_VERSION,
        id: session.id,
        account_id: session.account_id,
        recipient_public_key: session.recipient_public_key,
        recipient_device_id: session.recipient_device_id,
        recipient_device_name: session.recipient_device_name,
        approving_device_id: session.approving_device_id,
        expires_at: session.expires_at,
        approved: session.encrypted_payload.is_some(),
    }
}

#[utoipa::path(request_body = CreatePairingSession, responses((status = CREATED, body = PairingSessionResponse)))]
#[post("")]
async fn create_pairing_session(
    request: HttpRequest,
    pool: WebData,
    limiter: web::Data<RateLimiter>,
    form: web::Json<CreatePairingSession>,
) -> HandlerResult<impl Responder> {
    if !request_within_rate_limit(&request, &limiter) {
        return Err(HandlerError::TooManyRequests);
    }
    validate_create(&form)?;
    let now = now_ms()?;
    let session = PairingSession {
        id: form.id.clone(),
        version: i16::from(form.version),
        account_id: None,
        recipient_public_key: form.recipient_public_key.clone(),
        recipient_device_id: form.recipient_device_id.clone(),
        recipient_device_name: form.recipient_device_name.clone(),
        recipient_token_hash: form.recipient_token_hash.clone(),
        approving_device_id: None,
        encrypted_payload: None,
        expires_at: now + PAIRING_TTL_MS,
    };
    let mut conn = get_db_conn!(pool);
    match pairing::create(&mut conn, &session, now)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?
    {
        pairing::CreateResult::Created => Ok(HttpResponse::Created().json(response(session))),
        pairing::CreateResult::Duplicate => Err(HandlerError::PairingConflict),
        pairing::CreateResult::LimitExceeded => Err(HandlerError::PairingLimitExceeded),
    }
}

#[utoipa::path(responses((status = OK, body = PairingSessionResponse)))]
#[get("/{id}")]
async fn get_pairing_session(
    request: HttpRequest,
    pool: WebData,
    id: web::Path<String>,
) -> HandlerResult<impl Responder> {
    validate_session_id(&id)?;
    let token_hash = recipient_token_hash(&request)?;
    let mut conn = get_db_conn!(pool);
    let session = pairing::get(&mut conn, &id, &token_hash, now_ms()?)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?
        .ok_or(HandlerError::PairingNotFound)?;
    Ok(web::Json(response(session)))
}

#[utoipa::path(request_body = ClaimPairingSession, responses((status = OK, body = PairingClaimResponse)), security(("api_jwt_token" = [])))]
#[post("/{id}/claim")]
async fn claim_pairing_session(
    request: HttpRequest,
    pool: WebData,
    id: web::Path<String>,
    form: web::Json<ClaimPairingSession>,
) -> HandlerResult<impl Responder> {
    let account = authenticate_account(&request, &pool).await?;
    check_account_rate_limit(&account)?;
    validate_session_id(&id)?;
    validate_claim(&form)?;
    let candidate = PairingSession {
        id: id.into_inner(),
        version: i16::from(form.version),
        account_id: None,
        recipient_public_key: form.recipient_public_key.clone(),
        recipient_device_id: form.recipient_device_id.clone(),
        recipient_device_name: form.recipient_device_name.clone(),
        recipient_token_hash: String::new(),
        approving_device_id: None,
        encrypted_payload: None,
        expires_at: 0,
    };
    let mut conn = get_db_conn!(pool);
    let account_session = new_pairing_account_session(
        &account,
        candidate.id.clone(),
        candidate.recipient_device_id.clone(),
        form.encrypted_device_info.clone(),
    )?;
    let (session, account_session) = match pairing::claim(
        &mut conn,
        &account.id,
        &candidate,
        &account_session,
        now_ms()?,
    )
    .await
    .map_err(|_| HandlerError::InternalDatabaseError)?
    {
        pairing::ClaimResult::Claimed {
            pairing,
            account_session,
        } => (pairing, account_session),
        pairing::ClaimResult::Conflict => return Err(HandlerError::PairingConflict),
        pairing::ClaimResult::LimitExceeded => return Err(HandlerError::PairingLimitExceeded),
    };
    let jwt = session_token(&account, &account_session)?;
    Ok(web::Json(PairingClaimResponse {
        session: response(*session),
        jwt,
    }))
}

#[utoipa::path(request_body = ApprovePairingSession, responses((status = NO_CONTENT)), security(("api_jwt_token" = [])))]
#[put("/{id}")]
async fn approve_pairing_session(
    request: HttpRequest,
    pool: WebData,
    id: web::Path<String>,
    form: web::Json<ApprovePairingSession>,
) -> HandlerResult<impl Responder> {
    let account = authenticate_account(&request, &pool).await?;
    check_account_rate_limit(&account)?;
    validate_session_id(&id)?;
    validate_approval(&form)?;
    let mut conn = get_db_conn!(pool);
    let approved = pairing::approve(
        &mut conn,
        &account.id,
        &id,
        &form.approving_device_id,
        &form.encrypted_payload,
        now_ms()?,
    )
    .await
    .map_err(|_| HandlerError::InternalDatabaseError)?;
    if !approved {
        return Err(HandlerError::PairingConflict);
    }
    Ok(HttpResponse::NoContent())
}

#[utoipa::path(responses((status = OK, body = PairingPayloadResponse)))]
#[post("/{id}/consume")]
async fn consume_pairing_session(
    request: HttpRequest,
    pool: WebData,
    id: web::Path<String>,
) -> HandlerResult<impl Responder> {
    validate_session_id(&id)?;
    let token_hash = recipient_token_hash(&request)?;
    let mut conn = get_db_conn!(pool);
    let session = pairing::consume(&mut conn, &id, &token_hash, now_ms()?)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?
        .ok_or(HandlerError::PairingNotFound)?;
    Ok(web::Json(PairingPayloadResponse {
        approving_device_id: session
            .approving_device_id
            .ok_or(HandlerError::InternalDatabaseError)?,
        encrypted_payload: session
            .encrypted_payload
            .ok_or(HandlerError::InternalDatabaseError)?,
    }))
}

#[utoipa::path(responses((status = NO_CONTENT)))]
#[delete("/{id}")]
async fn cancel_pairing_session(
    request: HttpRequest,
    pool: WebData,
    id: web::Path<String>,
) -> HandlerResult<impl Responder> {
    validate_session_id(&id)?;
    let token_hash = recipient_token_hash(&request)?;
    let mut conn = get_db_conn!(pool);
    if !pairing::cancel(&mut conn, &id, &token_hash)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?
    {
        return Err(HandlerError::PairingNotFound);
    }
    Ok(HttpResponse::NoContent())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use actix_web::{App, http::StatusCode, test as actix_test};
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use utoipa_actix_web::{AppExt, scope};

    use super::{
        ApprovePairingSession, ClaimPairingSession, CreatePairingSession, PairingHandler,
        validate_approval, validate_claim, validate_create,
    };
    use super::{MAX_PAIRING_REQUESTS_PER_MINUTE, PairingRateLimiter};
    use crate::handlers::{ScopedHandler, encrypted_sync::EncryptedSyncHandler};

    fn encoded(byte: u8, length: usize) -> String {
        URL_SAFE_NO_PAD.encode(vec![byte; length])
    }

    #[test]
    fn accepts_canonical_pairing_fields() {
        let create = CreatePairingSession {
            version: 1,
            id: encoded(0, 32),
            recipient_public_key: encoded(1, 32),
            recipient_device_id: encoded(2, 16),
            recipient_device_name: "Living room laptop".to_owned(),
            recipient_token_hash: encoded(3, 32),
        };
        assert!(validate_create(&create).is_ok());
        let claim = ClaimPairingSession {
            version: create.version,
            recipient_public_key: create.recipient_public_key.clone(),
            recipient_device_id: create.recipient_device_id.clone(),
            recipient_device_name: create.recipient_device_name.clone(),
            encrypted_device_info: Some("encrypted-device-info".to_owned()),
        };
        assert!(validate_claim(&claim).is_ok());

        let approval = ApprovePairingSession {
            approving_device_id: encoded(4, 16),
            encrypted_payload: encoded(5, 256),
        };
        assert!(validate_approval(&approval).is_ok());
    }

    #[test]
    fn rejects_noncanonical_or_oversized_pairing_fields() {
        let create = CreatePairingSession {
            version: 1,
            id: "A".repeat(42),
            recipient_public_key: encoded(1, 32),
            recipient_device_id: encoded(2, 16),
            recipient_device_name: " device ".to_owned(),
            recipient_token_hash: encoded(3, 32),
        };
        assert!(validate_create(&create).is_err());

        let approval = ApprovePairingSession {
            approving_device_id: encoded(4, 16),
            encrypted_payload: "=".repeat(256),
        };
        assert!(validate_approval(&approval).is_err());
    }

    #[test]
    fn pairing_requests_are_rate_limited_per_account() {
        let limiter = PairingRateLimiter {
            windows: Mutex::new(HashMap::new()),
        };
        for _ in 0..MAX_PAIRING_REQUESTS_PER_MINUTE {
            assert!(limiter.check("account-a"));
        }
        assert!(!limiter.check("account-a"));
        assert!(limiter.check("account-b"));
    }

    #[actix_web::test]
    async fn pairing_route_is_reachable_after_encrypted_sync_routes() {
        let (app, _) = App::new()
            .into_utoipa_app()
            .service(
                scope::scope("/v1")
                    .service(EncryptedSyncHandler::get_service())
                    .service(PairingHandler::get_service()),
            )
            .split_for_parts();
        let app = actix_test::init_service(app).await;

        let request = actix_test::TestRequest::get()
            .uri("/v1/pairing/not-a-session")
            .to_request();
        let status = match actix_test::try_call_service(&app, request).await {
            Ok(response) => response.status(),
            Err(error) => error.as_response_error().status_code(),
        };

        assert_ne!(status, StatusCode::NOT_FOUND);
    }
}
