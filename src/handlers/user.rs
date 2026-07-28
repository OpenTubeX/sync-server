use std::net::{IpAddr, SocketAddr};

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::middleware::Next;
use actix_web::{HttpMessage, HttpResponse, Responder, delete, post, web};
use diesel::result::DatabaseErrorKind;
use utoipa_actix_web::scope;
use uuid::Uuid;

use crate::auth::{generate_jwt, hash_accountname, hash_password, verify_jwt, verify_password};
use crate::database::account::{
    delete_existing_account, find_account_by_id, find_account_by_name_hash, insert_new_account,
};
use crate::database::encrypted_sync;
use crate::dto::LoginResponse;
use crate::handlers::{HandlerError, HandlerResult, ScopedHandler};
use crate::models::Account;
use crate::rate_limit::RateLimiter;
use crate::{CONFIG, WebData, dto, get_db_conn, models};

const AUTH_HEADER_KEY: &str = "Authorization";
const MIN_PASSWORD_LENGTH: usize = 8;

/// Marker registered on scopes that remain reachable after an account has
/// switched to encrypted sync.
///
/// Everything else is refused for such accounts, so that an older client cannot
/// repopulate readable plaintext data. Add this only to scopes that either store
/// no plaintext sync data or are needed to manage the account itself.
pub struct PlaintextSyncExempt;

pub struct UserHandler {}
impl ScopedHandler for UserHandler {
    fn get_service() -> scope::Scope<
        impl ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<impl MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        >,
    > {
        // register and login are reachable without credentials, so the whole
        // scope is rate limited to blunt password guessing and registration spam.
        // Note this must wrap the outer scope: an extra nested `scope("")` would
        // match the prefix of, and therefore swallow, its sibling's routes.
        scope::scope("/account")
            .app_data(web::Data::new(
                RateLimiter::default().trusting_forwarded_for(CONFIG.trust_forwarded_for),
            ))
            .wrap(actix_web::middleware::from_fn(rate_limit_middleware))
            .service(register_account)
            .service(login_account)
            // services that require auth start here
            .service(
                scope::scope("")
                    .app_data(web::Data::new(PlaintextSyncExempt))
                    .wrap(actix_web::middleware::from_fn(auth_middleware))
                    .service(delete_account),
            )
    }
}

#[utoipa::path(responses((status = OK, body = LoginResponse)))]
#[post("/register")]
async fn register_account(
    pool: WebData,
    form: web::Json<dto::RegisterUser>,
) -> HandlerResult<impl Responder> {
    if !CONFIG.allow_registration {
        return Err(HandlerError::RegistrationDisabled);
    }

    let mut conn = get_db_conn!(pool);

    let password_length = form.password.len();
    if password_length < MIN_PASSWORD_LENGTH {
        return Err(HandlerError::PasswordTooShort);
    }

    let account = models::Account {
        id: Uuid::now_v7().to_string(),
        name_hash: hash_accountname(&form.name, CONFIG.username_secret().as_bytes()),
        password_hash: hash_password(&form.password),
    };

    let account = insert_new_account(&mut conn, &account)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => {
                HandlerError::AccountNameTaken
            }
            _ => HandlerError::InternalDatabaseErrorWithContext(err.to_string()),
        })?;

    match generate_jwt(&account, CONFIG.secret.as_bytes()) {
        Ok(jwt) => {
            let resp = LoginResponse { jwt };
            Ok(HttpResponse::Created().json(resp))
        }
        Err(err) => Err(HandlerError::InternalDatabaseErrorWithContext(
            err.to_string(),
        )),
    }
}

#[utoipa::path(responses((status = CREATED, body = LoginResponse)))]
#[post("/login")]
async fn login_account(
    pool: WebData,
    form: web::Json<dto::LoginUser>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    let name = hash_accountname(&form.name, CONFIG.username_secret().as_bytes());
    let Some(account) = find_account_by_name_hash(&mut conn, &name)
        .await
        .ok()
        .flatten()
    else {
        return Err(HandlerError::InvalidCredentials);
    };

    if !verify_password(&form.password, &account.password_hash) {
        return Err(HandlerError::InvalidCredentials);
    }

    match generate_jwt(&account, CONFIG.secret.as_bytes()) {
        Ok(jwt) => {
            let resp = LoginResponse { jwt };
            Ok(HttpResponse::Ok().json(resp))
        }
        Err(err) => Err(HandlerError::InternalDatabaseErrorWithContext(
            err.to_string(),
        )),
    }
}

#[utoipa::path(responses((status = OK)), security(("api_jwt_token" = [])))]
#[delete("/delete")]
async fn delete_account(
    account: Account,
    pool: WebData,
    form: web::Json<dto::DeleteUser>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    if !verify_password(&form.password, &account.password_hash) {
        return Err(HandlerError::InvalidCredentials);
    }

    match delete_existing_account(&mut conn, &account.id).await {
        Ok(_) => Ok(HttpResponse::Ok()),
        Err(err) => Err(HandlerError::InternalDatabaseErrorWithContext(
            err.to_string(),
        )),
    }
}

/// Middleware that rate limits unauthenticated endpoints per client address.
///
/// Defaults to the peer address, since forwarded headers are client-controlled
/// when the server is directly reachable. Behind a reverse proxy the peer is the
/// proxy itself, which would put every client in one bucket, so
/// `trust_forwarded_for` switches to the forwarded address instead.
pub async fn rate_limit_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let limiter: Option<&web::Data<RateLimiter>> = req.app_data();

    if let Some(limiter) = limiter
        && let Some(client) = rate_limit_client(&req, limiter.trusts_forwarded_for())
        && !limiter.check(client)
    {
        return Err(HandlerError::TooManyRequests.into());
    }

    next.call(req).await
}

/// Address to rate limit a request against.
fn rate_limit_client(req: &ServiceRequest, trust_forwarded_for: bool) -> Option<IpAddr> {
    let peer = req.peer_addr().map(|addr| addr.ip());

    if !trust_forwarded_for {
        return peer;
    }

    req.headers()
        .get("X-Forwarded-For")
        .and_then(|value| value.to_str().ok())
        .and_then(forwarded_client)
        .or(peer)
}

/// Client address from an `X-Forwarded-For` value.
///
/// Takes the *last* entry, because a proxy appends the address it observed. The
/// earlier entries are whatever the client sent, so trusting the first one would
/// let a client pick its own bucket and bypass the limit entirely.
fn forwarded_client(header: &str) -> Option<IpAddr> {
    header
        .rsplit(',')
        .map(str::trim)
        .find(|entry| !entry.is_empty())
        .and_then(|entry| {
            entry
                .parse::<IpAddr>()
                .ok()
                // tolerate `host:port` forms
                .or_else(|| entry.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
        })
}

/// Middleware that ensures that the account is authenticated.
pub async fn auth_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let auth_header = req
        .headers()
        .get(AUTH_HEADER_KEY)
        .and_then(|header| header.to_str().ok())
        .map(|value| value.to_string());
    let auth_cookie = req
        .cookie(AUTH_HEADER_KEY)
        .map(|cookie| cookie.value().to_string());

    let Some(jwt) = auth_cookie.or(auth_header) else {
        return Err(HandlerError::InvalidToken.into());
    };
    let Ok(account_id) = verify_jwt(&jwt, CONFIG.secret.as_bytes()) else {
        return Err(HandlerError::InvalidToken.into());
    };

    let pool: WebData = req.app_data().cloned().unwrap();
    let mut conn = get_db_conn!(pool);

    let Some(account) = find_account_by_id(&mut conn, &account_id)
        .await
        .ok()
        .flatten()
    else {
        return Err(HandlerError::AccountNotExists.into());
    };

    // Scopes opt out explicitly. Matching on the request path instead would let
    // a route parameter such as `/playlists/encrypted_sync/videos` slip past.
    let exempt = req.app_data::<web::Data<PlaintextSyncExempt>>().is_some();
    if !exempt {
        let encrypted_sync_enabled = encrypted_sync::exists(&mut conn, &account.id)
            .await
            .map_err(|_| HandlerError::InternalDatabaseError)?;
        if encrypted_sync_enabled {
            let migration_complete = !encrypted_sync::has_legacy_data(&mut conn, &account.id)
                .await
                .map_err(|_| HandlerError::InternalDatabaseError)?;
            if migration_complete {
                return Err(HandlerError::EncryptedSyncRequired.into());
            }
        }
    }

    // Do not hold a pooled connection while the endpoint acquires its own.
    drop(conn);

    // append account to request extensions so that it can be accessed with
    // `req.extensions().get::<User>()` by handlers
    req.extensions_mut().insert(account);

    next.call(req).await
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use actix_web::{App, HttpResponse, Responder, get, http::StatusCode, test, web};

    use super::{PlaintextSyncExempt, rate_limit_middleware};
    use crate::rate_limit::RateLimiter;

    // Nested so that `actix_web::test` imported above does not shadow the
    // built-in `#[test]` attribute for these synchronous tests.
    mod forwarded {
        use crate::handlers::user::forwarded_client;

        fn resolved(header: &str) -> Option<String> {
            forwarded_client(header).map(|ip| ip.to_string())
        }

        /// The last entry is the one a proxy appended; earlier entries are
        /// whatever the client sent and must not be trusted.
        #[test]
        fn forwarded_client_uses_the_last_entry() {
            assert_eq!(resolved("203.0.113.5").as_deref(), Some("203.0.113.5"));
            // a client-supplied spoof followed by the address the proxy saw
            assert_eq!(
                resolved("1.1.1.1, 203.0.113.5").as_deref(),
                Some("203.0.113.5")
            );
            assert_eq!(
                resolved("1.1.1.1, 203.0.113.5:44321").as_deref(),
                Some("203.0.113.5")
            );
        }

        #[test]
        fn forwarded_client_ignores_unusable_values() {
            assert_eq!(resolved(""), None);
            assert_eq!(resolved("not-an-address"), None);
            assert_eq!(resolved("1.1.1.1, "), Some("1.1.1.1".to_owned()));
        }
    }

    #[get("/ping")]
    async fn ping() -> impl Responder {
        HttpResponse::Ok()
    }

    #[actix_web::post("/register")]
    async fn stub_register() -> impl Responder {
        HttpResponse::Created()
    }

    #[actix_web::delete("/delete")]
    async fn stub_delete() -> impl Responder {
        HttpResponse::NoContent()
    }

    /// Mirrors the real `/account` layout. An extra nested `scope("")` around
    /// register/login would match the prefix of its sibling and swallow
    /// `/account/delete`, so both routes must stay reachable.
    #[actix_rt::test]
    async fn both_account_routes_stay_routable() {
        let app = test::init_service(
            App::new().service(
                web::scope("/account")
                    .app_data(web::Data::new(RateLimiter::default()))
                    .wrap(actix_web::middleware::from_fn(rate_limit_middleware))
                    .service(stub_register)
                    .service(web::scope("").service(stub_delete)),
            ),
        )
        .await;

        let register = test::TestRequest::post()
            .uri("/account/register")
            .peer_addr("203.0.113.9:5000".parse().unwrap())
            .to_request();
        assert_eq!(
            test::call_service(&app, register).await.status(),
            StatusCode::CREATED
        );

        let delete = test::TestRequest::delete()
            .uri("/account/delete")
            .peer_addr("203.0.113.9:5000".parse().unwrap())
            .to_request();
        assert_eq!(
            test::call_service(&app, delete).await.status(),
            StatusCode::NO_CONTENT
        );
    }

    /// Mirrors how `auth_middleware` decides whether a scope is exempt from the
    /// encrypted-sync requirement, without needing a database.
    async fn exemption_probe(
        req: actix_web::dev::ServiceRequest,
        next: actix_web::middleware::Next<impl actix_web::body::MessageBody>,
    ) -> Result<actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>, actix_web::Error>
    {
        if req.app_data::<web::Data<PlaintextSyncExempt>>().is_none() {
            return Err(crate::handlers::HandlerError::EncryptedSyncRequired.into());
        }

        next.call(req).await
    }

    /// A playlist (or video) named `encrypted_sync` used to make the old
    /// `path().contains("/encrypted_sync")` check exempt a plaintext route.
    #[actix_rt::test]
    async fn plaintext_routes_are_not_exempted_by_their_path() {
        let app = test::init_service(
            App::new()
                .service(
                    web::scope("/encrypted_sync")
                        .app_data(web::Data::new(PlaintextSyncExempt))
                        .wrap(actix_web::middleware::from_fn(exemption_probe))
                        .service(ping),
                )
                .service(
                    web::scope("/playlists")
                        .wrap(actix_web::middleware::from_fn(exemption_probe))
                        .service(ping),
                ),
        )
        .await;

        let status = |uri: &'static str| async {
            let req = test::TestRequest::get().uri(uri).to_request();
            match test::try_call_service(&app, req).await {
                Ok(response) => response.status(),
                Err(error) => error.as_response_error().status_code(),
            }
        };

        // the genuinely encrypted scope stays reachable
        assert_eq!(status("/encrypted_sync/ping").await, StatusCode::OK);
        // a plaintext route is refused even though its path contains the marker word
        assert_eq!(
            status("/playlists/encrypted_sync/ping").await,
            StatusCode::CONFLICT
        );
    }

    /// Behind a proxy every request shares one peer address, so distinct
    /// forwarded clients must get their own budgets once the header is trusted.
    #[actix_rt::test]
    async fn forwarded_clients_are_bucketed_separately() {
        let app = test::init_service(
            App::new().service(
                web::scope("/t")
                    .app_data(web::Data::new(
                        RateLimiter::new(1, Duration::from_secs(3600)).trusting_forwarded_for(true),
                    ))
                    .wrap(actix_web::middleware::from_fn(rate_limit_middleware))
                    .service(ping),
            ),
        )
        .await;

        // same proxy peer for every request, different real clients
        let proxy: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let expected = [
            ("203.0.113.1", StatusCode::OK),
            // a different client is not punished for the first one's request
            ("203.0.113.2", StatusCode::OK),
            // but the first client's own budget is spent
            ("203.0.113.1", StatusCode::TOO_MANY_REQUESTS),
        ];

        for (client, want) in expected {
            let req = test::TestRequest::get()
                .uri("/t/ping")
                .peer_addr(proxy)
                .insert_header(("X-Forwarded-For", client))
                .to_request();
            let got = match test::try_call_service(&app, req).await {
                Ok(response) => response.status(),
                Err(error) => error.as_response_error().status_code(),
            };
            assert_eq!(got, want, "client {client}");
        }
    }

    /// Guards against the middleware silently failing open, which would happen
    /// if scope-level `app_data` were not visible to scope-level middleware.
    #[actix_rt::test]
    async fn rate_limit_middleware_rejects_a_burst() {
        let app = test::init_service(
            App::new().service(
                web::scope("/t")
                    .app_data(web::Data::new(RateLimiter::new(
                        2,
                        Duration::from_secs(3600),
                    )))
                    .wrap(actix_web::middleware::from_fn(rate_limit_middleware))
                    .service(ping),
            ),
        )
        .await;

        let peer: SocketAddr = "203.0.113.7:5000".parse().unwrap();
        let mut statuses = Vec::new();
        for _ in 0..3 {
            let req = test::TestRequest::get()
                .uri("/t/ping")
                .peer_addr(peer)
                .to_request();
            // the middleware signals rejection with an error, which actix renders
            // through `ResponseError` in a real server
            statuses.push(match test::try_call_service(&app, req).await {
                Ok(response) => response.status(),
                Err(error) => error.as_response_error().status_code(),
            });
        }

        assert_eq!(statuses[0], StatusCode::OK);
        assert_eq!(statuses[1], StatusCode::OK);
        assert_eq!(statuses[2], StatusCode::TOO_MANY_REQUESTS);
    }
}
