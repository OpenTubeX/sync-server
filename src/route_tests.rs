use actix_web::{
    App, Error,
    body::{MessageBody, to_bytes},
    dev::ServiceResponse,
    http::{Method, StatusCode},
    test, web,
};
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, bb8::Pool};
use diesel_migrations::MigrationHarness;
use serde_json::{Value, json};
use utoipa_actix_web::AppExt;
use utoipa_scalar::{Scalar, Servable};

use crate::{
    CONFIG, DbConnection, DbPool, MIGRATIONS,
    auth::{generate_jwt, hash_password, unix_now},
    configure_api_routes,
    database::{account, account_session},
    handlers::{ScopedHandler, health::HealthHandler, session::now_ms},
    models::{Account, AccountSession},
    rate_limit::RateLimiter,
};

async fn pool() -> DbPool {
    let pool = Pool::builder()
        .max_size(1)
        .build(AsyncDieselConnectionManager::<DbConnection>::new(
            ":memory:",
        ))
        .await
        .unwrap();
    pool.get()
        .await
        .unwrap()
        .spawn_blocking(|conn| {
            use diesel::connection::SimpleConnection;
            conn.batch_execute("PRAGMA foreign_keys = ON;")?;
            conn.run_pending_migrations(MIGRATIONS).unwrap();
            Ok(())
        })
        .await
        .unwrap();
    pool
}

async fn seed_account(pool: &DbPool) -> String {
    static PASSWORD_HASH: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| hash_password("test-password"));
    let account = Account {
        id: "account".into(),
        name_hash: "name-hash".into(),
        password_hash: Some(PASSWORD_HASH.clone()),
        oidc_sub: None,
        legacy_tokens_enabled: false,
        session_generation: 0,
    };
    let now = now_ms().unwrap();
    let session = AccountSession {
        id: "session".into(),
        account_id: account.id.clone(),
        device_id: "AAAAAAAAAAAAAAAAAAAAAA".into(),
        encrypted_device_info: Some("encrypted-device-info".into()),
        created_at: now,
        last_active_at: now,
        expires_at: (unix_now() + 3600) as i64 * 1000,
        revoked_at: None,
        legacy: false,
        generation: 0,
        pending_pairing: false,
    };
    let mut conn = pool.get().await.unwrap();
    account::insert_new_account(&mut conn, &account)
        .await
        .unwrap();
    account_session::create(&mut conn, &session).await.unwrap();
    generate_jwt(
        &account,
        &session.id,
        unix_now() + 3600,
        CONFIG.secret.as_bytes(),
    )
    .unwrap()
}

// Middleware errors become HTTP responses at the server boundary. Actix's test
// service returns them as Err, so compare their actual HTTP representation too.
async fn response_snapshot(
    response: Result<ServiceResponse<impl MessageBody + 'static>, Error>,
) -> (StatusCode, Vec<(String, Vec<u8>)>, web::Bytes) {
    let response = match response {
        Ok(response) => response.into_parts().1.map_into_boxed_body(),
        Err(error) => error.error_response(),
    };
    let status = response.status();
    let mut headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
        .collect::<Vec<_>>();
    headers.sort();
    let body = to_bytes(response.into_body()).await.unwrap();
    (status, headers, body)
}

#[actix_web::test]
async fn all_v1_routes_have_matching_aliases() {
    let (app, api) = App::new()
        .into_utoipa_app()
        .app_data(web::Data::new(pool().await))
        .app_data(web::Data::new(RateLimiter::default()))
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(app).await;
    let api = serde_json::to_value(api).unwrap();
    let paths = api["paths"].as_object().unwrap();
    assert!(paths.contains_key("/v1/account/delete"));
    assert!(paths.contains_key("/v1/account/sessions"));
    for (path, operations) in paths {
        let Some(alias) = path.strip_prefix("/v1") else {
            panic!("OpenAPI should document canonical routes only: {path}");
        };
        let alias = alias
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    "test-id"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        for method in ["get", "post", "put", "patch", "delete"] {
            if operations.get(method).is_none() {
                continue;
            }
            for token in [None, Some("invalid-token")] {
                let mut responses = Vec::new();
                for uri in [format!("/v1{alias}"), alias.clone()] {
                    let mut request = test::TestRequest::default()
                        .method(Method::from_bytes(method.to_uppercase().as_bytes()).unwrap())
                        .uri(&uri)
                        .set_json(json!({}));
                    if let Some(token) = token {
                        request = request.insert_header(("Authorization", token));
                    }
                    responses.push(
                        response_snapshot(test::try_call_service(&app, request.to_request()).await)
                            .await,
                    );
                }
                if path == "/v1/account/delete" || path == "/v1/account/sessions" {
                    assert_eq!(responses[0].0, StatusCode::UNAUTHORIZED);
                }
                assert!(!responses[0].0.is_server_error(), "{method} {path}");
                assert_eq!(
                    responses[0], responses[1],
                    "{method} {alias}, token={token:?}"
                );
            }
        }
    }
}

#[actix_web::test]
async fn account_sessions_alias_matches_success_and_revoked_authentication() {
    let pool = pool().await;
    let token = seed_account(&pool).await;
    let (app, _) = App::new()
        .into_utoipa_app()
        .app_data(web::Data::new(pool.clone()))
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(app).await;
    let mut responses = Vec::new();
    for uri in ["/v1/account/sessions", "/account/sessions"] {
        let request = test::TestRequest::get()
            .uri(uri)
            .insert_header(("Authorization", token.clone()))
            .to_request();
        let response = response_snapshot(test::try_call_service(&app, request).await).await;
        assert_eq!(response.0, StatusCode::OK);
        let body: Value = serde_json::from_slice(&response.2).unwrap();
        assert_eq!(body["password_login"], true);
        assert_eq!(body["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(body["sessions"][0]["id"], "session");
        assert_eq!(body["sessions"][0]["current"], true);
        responses.push(response);
    }
    assert_eq!(responses[0], responses[1]);

    account_session::revoke(
        &mut pool.get().await.unwrap(),
        "account",
        "session",
        0,
        now_ms().unwrap(),
    )
    .await
    .unwrap();
    let mut failures = Vec::new();
    for (method, path) in [(Method::GET, "sessions"), (Method::DELETE, "delete")] {
        for prefix in ["/v1", ""] {
            let request = test::TestRequest::default()
                .method(method.clone())
                .uri(&format!("{prefix}/account/{path}"))
                .insert_header(("Authorization", token.clone()))
                .set_json(json!({"password": "test-password"}))
                .to_request();
            let response = response_snapshot(test::try_call_service(&app, request).await).await;
            assert_eq!(response.0, StatusCode::UNAUTHORIZED);
            failures.push(response);
        }
    }
    assert!(failures.windows(2).all(|pair| pair[0] == pair[1]));
}

#[actix_web::test]
async fn delete_account_alias_matches_validation_and_deletes_account() {
    let mut responses = Vec::new();
    for uri in ["/v1/account/delete", "/account/delete"] {
        let pool = pool().await;
        let token = seed_account(&pool).await;
        let (app, _) = App::new()
            .into_utoipa_app()
            .app_data(web::Data::new(pool.clone()))
            .configure(configure_api_routes)
            .split_for_parts();
        let app = test::init_service(app).await;
        let mut route_responses = Vec::new();
        for (payload, status) in [
            ("{}", StatusCode::BAD_REQUEST),
            ("{", StatusCode::BAD_REQUEST),
            (r#"{"password":"wrong"}"#, StatusCode::FORBIDDEN),
            (r#"{"password":"test-password"}"#, StatusCode::OK),
        ] {
            let request = test::TestRequest::delete()
                .uri(uri)
                .insert_header(("Authorization", token.clone()))
                .insert_header(("Content-Type", "application/json"))
                .set_payload(payload)
                .to_request();
            let response = response_snapshot(test::try_call_service(&app, request).await).await;
            assert_eq!(response.0, status, "{uri}: {payload}");
            assert_eq!(
                account::find_account_by_id(&mut pool.get().await.unwrap(), "account")
                    .await
                    .unwrap()
                    .is_none(),
                status == StatusCode::OK
            );
            route_responses.push(response);
        }
        responses.push(route_responses);
    }
    assert_eq!(responses[0], responses[1]);
}

#[actix_web::test]
async fn aliases_share_rate_limits_and_preserve_other_routes() {
    let pool = pool().await;
    let token = seed_account(&pool).await;
    let (app, api) = App::new()
        .into_utoipa_app()
        .app_data(web::Data::new(pool))
        .app_data(web::Data::new(RateLimiter::new(
            1,
            std::time::Duration::from_secs(3600),
        )))
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(
        app.service(Scalar::with_url("/docs", api))
            .service(HealthHandler::get_service()),
    )
    .await;
    for (uri, status) in [
        ("/v1/account/delete", StatusCode::FORBIDDEN),
        ("/account/delete", StatusCode::TOO_MANY_REQUESTS),
    ] {
        let request = test::TestRequest::delete()
            .uri(uri)
            .peer_addr("203.0.113.9:5000".parse().unwrap())
            .insert_header(("Authorization", token.clone()))
            .set_json(json!({"password": "wrong"}))
            .to_request();
        assert_eq!(
            response_snapshot(test::try_call_service(&app, request).await)
                .await
                .0,
            status
        );
    }
    // Session listing remains exempt even after the shared attempt budget is spent.
    for uri in ["/v1/account/sessions", "/account/sessions"] {
        let request = test::TestRequest::get()
            .uri(uri)
            .peer_addr("203.0.113.9:5000".parse().unwrap())
            .insert_header(("Authorization", token.clone()))
            .to_request();
        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::OK
        );
    }
    for uri in ["/", "/health", "/healthz", "/docs"] {
        let response =
            test::call_service(&app, test::TestRequest::get().uri(uri).to_request()).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            response
                .request()
                .url_for_static("delete_account")
                .unwrap()
                .path(),
            "/v1/account/delete"
        );
    }
    for uri in ["/unknown", "/v2/account/sessions", "/v1/unknown"] {
        assert_eq!(
            test::call_service(&app, test::TestRequest::get().uri(uri).to_request())
                .await
                .status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}
