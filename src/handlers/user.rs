use std::net::{IpAddr, SocketAddr};

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::middleware::Next;
use actix_web::web::Redirect;
use actix_web::{HttpMessage, HttpRequest, HttpResponse, Responder, delete, get, post, web};
use diesel::result::DatabaseErrorKind;
use serde::Deserialize;
use utoipa_actix_web::scope;
use uuid::Uuid;

use crate::auth::{generate_jwt, hash_accountname, hash_password, verify_jwt, verify_password};
use crate::database::account::{
    delete_existing_account, delete_existing_account_by_oidc_sub, find_account_by_id,
    find_account_by_name_hash, insert_new_account,
};
use crate::database::encrypted_sync;
use crate::dto::LoginResponse;
use crate::handlers::{HandlerError, HandlerResult, ScopedHandler};
use crate::models::Account;
use crate::oidc::check_oidc_auth_request;
use crate::rate_limit::RateLimiter;
use crate::{CONFIG, WebData, dto, get_db_conn, models, oidc};

const AUTH_HEADER_KEY: &str = "Authorization";
const MIN_PASSWORD_LENGTH: usize = 8;
const OIDC_ACCOUNT_PREFIX: &str = "OIDC-ACCOUNT-";

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
        // The limiter itself is registered once on the App, not here: this
        // function runs per worker, so building it here would give every worker
        // its own counters and multiply the effective limit by the worker count.
        let mut account_scope = scope::scope("/account")
            .service(register_account)
            .service(login_account);

        if CONFIG.oidc.is_some() {
            account_scope = account_scope
                .service(authenticate_oidc_account)
                .service(authenticate_oidc_account_callback)
                .service(delete_oidc_account)
                .service(delete_oidc_account_callback)
        };

        // services that require auth start here
        account_scope
            .wrap(actix_web::middleware::from_fn(rate_limit_middleware))
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

    // usernames starting with OIDC_ACCOUNT_PREFIX are preserved for oidc users
    if form.name.starts_with(OIDC_ACCOUNT_PREFIX) {
        return Err(HandlerError::InvalidCredentials);
    }

    let mut conn = get_db_conn!(pool);

    let password_length = form.password.len();
    if password_length < MIN_PASSWORD_LENGTH {
        return Err(HandlerError::PasswordTooShort);
    }

    let account = models::Account {
        id: Uuid::now_v7().to_string(),
        name_hash: hash_accountname(&form.name, CONFIG.username_secret().as_bytes()),
        password_hash: Some(hash_password(&form.password)),
        oidc_sub: None,
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

    let Some(password_hash) = &account.password_hash else {
        return Err(HandlerError::PasswordLoginDisabledForAccount);
    };

    if !verify_password(&form.password, password_hash) {
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

    let Some(password_hash) = &account.password_hash else {
        return Err(HandlerError::PasswordLoginDisabledForAccount);
    };

    if !verify_password(&form.password, password_hash) {
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
        && let Some(client) = rate_limit_client(
            &req,
            limiter.trusts_forwarded_for(),
            limiter.trusted_proxy_hops(),
        )
        && !limiter.check(client)
    {
        return Err(HandlerError::TooManyRequests.into());
    }

    next.call(req).await
}

/// Address to rate limit a request against.
fn rate_limit_client(
    req: &ServiceRequest,
    trust_forwarded_for: bool,
    trusted_proxy_hops: usize,
) -> Option<IpAddr> {
    let peer = req.peer_addr().map(|addr| addr.ip());

    if !trust_forwarded_for {
        return peer;
    }

    req.headers()
        .get("X-Forwarded-For")
        .and_then(|value| value.to_str().ok())
        .and_then(|header| forwarded_client(header, trusted_proxy_hops))
        .or(peer)
}

/// Client address from an `X-Forwarded-For` value.
///
/// Counts back `trusted_proxy_hops` entries from the end, because each proxy
/// appends the address it observed. With one proxy that is the last entry; with
/// two it is the second to last, since the last is then the inner proxy's own
/// address. Entries further left are whatever the client sent, so counting from
/// the front would let a client pick its own bucket and bypass the limit.
fn forwarded_client(header: &str, trusted_proxy_hops: usize) -> Option<IpAddr> {
    header
        .rsplit(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .nth(trusted_proxy_hops.saturating_sub(1))
        .and_then(|entry| {
            entry
                .parse::<IpAddr>()
                .ok()
                // tolerate `host:port` forms
                .or_else(|| entry.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
        })
}

pub(crate) fn request_within_rate_limit(req: &HttpRequest, limiter: &RateLimiter) -> bool {
    let peer = req.peer_addr().map(|address| address.ip());
    let client = if limiter.trusts_forwarded_for() {
        req.headers()
            .get("X-Forwarded-For")
            .and_then(|value| value.to_str().ok())
            .and_then(|header| forwarded_client(header, limiter.trusted_proxy_hops()))
            .or(peer)
    } else {
        peer
    };
    client.is_none_or(|address| limiter.check(address))
}

pub(crate) async fn authenticate_account(
    req: &HttpRequest,
    pool: &WebData,
) -> HandlerResult<Account> {
    let auth_header = req
        .headers()
        .get(AUTH_HEADER_KEY)
        .and_then(|header| header.to_str().ok())
        .map(str::to_owned);
    let auth_cookie = req
        .cookie(AUTH_HEADER_KEY)
        .map(|cookie| cookie.value().to_owned());

    let jwt = auth_cookie
        .or(auth_header)
        .ok_or(HandlerError::InvalidToken)?;
    let account_id =
        verify_jwt(&jwt, CONFIG.secret.as_bytes()).map_err(|_| HandlerError::InvalidToken)?;
    let mut conn = get_db_conn!(pool);
    find_account_by_id(&mut conn, &account_id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?
        .ok_or(HandlerError::AccountNotExists)
}

/// Middleware that ensures that the account is authenticated.
pub async fn auth_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let pool: WebData = req.app_data().cloned().unwrap();
    let account = authenticate_account(req.request(), &pool).await?;
    let mut conn = get_db_conn!(pool);

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
            resolved_hops(header, 1)
        }

        fn resolved_hops(header: &str, hops: usize) -> Option<String> {
            forwarded_client(header, hops).map(|ip| ip.to_string())
        }

        /// With two proxies the last entry is the inner proxy, not the client.
        #[test]
        fn forwarded_client_honours_the_hop_count() {
            let header = "203.0.113.5, 10.0.0.7";
            assert_eq!(resolved_hops(header, 1).as_deref(), Some("10.0.0.7"));
            assert_eq!(resolved_hops(header, 2).as_deref(), Some("203.0.113.5"));
            // more hops than entries yields nothing, so the caller falls back
            assert_eq!(resolved_hops(header, 3), None);
            // zero is treated as one rather than panicking
            assert_eq!(resolved_hops(header, 0).as_deref(), Some("10.0.0.7"));
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

    /// Mirrors production wiring: the limiter is registered on the App, and the
    /// middleware on a scope. A limiter built inside the per-worker closure
    /// instead would give each worker its own counters.
    #[actix_rt::test]
    async fn app_level_limiter_is_visible_to_scope_middleware() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(RateLimiter::new(
                    1,
                    Duration::from_secs(3600),
                )))
                .service(
                    web::scope("/t")
                        .wrap(actix_web::middleware::from_fn(rate_limit_middleware))
                        .service(ping),
                ),
        )
        .await;

        let peer: SocketAddr = "203.0.113.20:5000".parse().unwrap();
        let call = |uri: &'static str| {
            test::TestRequest::get()
                .uri(uri)
                .peer_addr(peer)
                .to_request()
        };

        let first = match test::try_call_service(&app, call("/t/ping")).await {
            Ok(response) => response.status(),
            Err(error) => error.as_response_error().status_code(),
        };
        let second = match test::try_call_service(&app, call("/t/ping")).await {
            Ok(response) => response.status(),
            Err(error) => error.as_response_error().status_code(),
        };

        assert_eq!(first, StatusCode::OK);
        assert_eq!(second, StatusCode::TOO_MANY_REQUESTS);
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

#[derive(Deserialize)]
struct OidcAuthenticationRequest {
    /// Url to redirect to once authentication succeeded.
    /// Passes a `token` query parameter to the URL, which is a valid JWT for the authenticated account.
    redirect_url: String,
}

#[utoipa::path]
#[get("/oidc/authenticate")]
async fn authenticate_oidc_account(
    req: HttpRequest,
    query: web::Query<OidcAuthenticationRequest>,
) -> HandlerResult<impl Responder> {
    let callback_route = req
        .url_for::<&[_; 0], &String>("authenticate_oidc_account_callback", &[])
        .unwrap();

    let redirect_url = oidc::authenticate_oidc_user_request(
        &CONFIG.oidc.clone().unwrap(),
        callback_route.path(),
        query.redirect_url.clone(),
    )
    .await
    .map_err(HandlerError::OidcError)?;

    Ok(Redirect::to(redirect_url))
}

fn oidc_username_hash(oidc_sub: &str) -> String {
    // the name is getting hashed anyways, so its actual value isn't important because the user
    // never sees it
    // it only is important that the username never changes and doesn't conflict with the normally created ones
    let username = format!("{OIDC_ACCOUNT_PREFIX}{oidc_sub}");
    hash_accountname(&username, CONFIG.username_secret().as_bytes())
}

#[derive(Deserialize)]
struct OidcCallbackData {
    code: String,
    state: String,
}

#[utoipa::path]
#[get("/oidc/authenticate/callback")]
async fn authenticate_oidc_account_callback(
    pool: WebData,
    query: web::Query<OidcCallbackData>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    let (user_claims, redirect_url) = check_oidc_auth_request(&query.state, query.code.clone())
        .await
        .map_err(HandlerError::OidcError)?;

    let oidc_sub = user_claims.subject().as_str();

    let name_hash = oidc_username_hash(oidc_sub);
    let account = if let Some(existing_account) = find_account_by_name_hash(&mut conn, &name_hash)
        .await
        .ok()
        .flatten()
    {
        existing_account
    } else {
        let account = Account {
            id: Uuid::now_v7().to_string(),
            name_hash,
            // the password_hash field should be nullable instead of using an empty string here,
            // but unfortunately SQLite doesn't have a statement to alter table columns...
            password_hash: None,
            oidc_sub: Some(oidc_sub.to_string()),
        };
        insert_new_account(&mut conn, &account)
            .await
            .map_err(|err| HandlerError::InternalDatabaseErrorWithContext(err.to_string()))?;

        account
    };

    match generate_jwt(&account, CONFIG.secret.as_bytes()) {
        Ok(jwt) => Ok(Redirect::to(format!("{redirect_url}?token={jwt}"))),
        Err(err) => Err(HandlerError::InternalDatabaseErrorWithContext(
            err.to_string(),
        )),
    }
}

#[utoipa::path]
#[get("/oidc/delete")]
async fn delete_oidc_account(
    req: HttpRequest,
    query: web::Query<OidcAuthenticationRequest>,
) -> HandlerResult<impl Responder> {
    let callback_route = req
        .url_for::<&[_; 0], &String>("delete_oidc_account_callback", &[])
        .unwrap();

    let redirect_url = oidc::authenticate_oidc_user_request(
        &CONFIG.oidc.clone().unwrap(),
        callback_route.path(),
        query.redirect_url.clone(),
    )
    .await
    .map_err(HandlerError::OidcError)?;

    Ok(Redirect::to(redirect_url))
}

#[utoipa::path]
#[get("/oidc/delete/callback")]
async fn delete_oidc_account_callback(
    pool: WebData,
    query: web::Query<OidcCallbackData>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    let (user_claims, redirect_url) = check_oidc_auth_request(&query.state, query.code.clone())
        .await
        .map_err(HandlerError::OidcError)?;

    let oidc_sub = user_claims.subject().as_str();

    match delete_existing_account_by_oidc_sub(&mut conn, oidc_sub).await {
        Ok(deleted) => {
            if deleted {
                Ok(Redirect::to(redirect_url))
            } else {
                Err(HandlerError::AccountNotExists)
            }
        }
        Err(err) => Err(HandlerError::InternalDatabaseErrorWithContext(
            err.to_string(),
        )),
    }
}
