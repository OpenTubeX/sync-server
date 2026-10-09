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
async fn cors_preflights_allow_client_headers_without_authentication() {
    let (app, _) = App::new()
        .into_utoipa_app()
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(
        app.service(HealthHandler::get_service())
            .wrap(actix_web::middleware::from_fn(crate::cors::cors_middleware)),
    )
    .await;

    for origin in ["null", "https://client.example", "capacitor://localhost"] {
        for (uri, method, headers) in [
            ("/health", "GET", "opentubex-client-version"),
            (
                "/v1/encrypted_sync/settings",
                "PUT",
                "authorization,content-type,opentubex-client-version",
            ),
            (
                "/encrypted_sync/settings",
                "PUT",
                "authorization,content-type,opentubex-client-version",
            ),
            (
                "/v1/pairing/test",
                "GET",
                "x-pairing-token,opentubex-client-version",
            ),
            (
                "/v1/account/sessions/test",
                "PATCH",
                "authorization,content-type",
            ),
            ("/account/delete", "DELETE", "authorization"),
            (
                "/account/login",
                "POST",
                "content-type,opentubex-client-version",
            ),
        ] {
            let request = test::TestRequest::default()
                .method(Method::OPTIONS)
                .uri(uri)
                .insert_header(("Origin", origin))
                .insert_header(("Access-Control-Request-Method", method))
                .insert_header(("Access-Control-Request-Headers", headers))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "OPTIONS {uri}");
            assert_eq!(
                response
                    .headers()
                    .get("Access-Control-Allow-Origin")
                    .unwrap(),
                "*"
            );
            let mut allowed_methods = response
                .headers()
                .get("Access-Control-Allow-Methods")
                .unwrap()
                .to_str()
                .unwrap()
                .split(',')
                .map(str::trim)
                .collect::<Vec<_>>();
            allowed_methods.sort_unstable();
            assert_eq!(
                allowed_methods,
                ["DELETE", "GET", "HEAD", "PATCH", "POST", "PUT"]
            );
            let mut allowed_headers = response
                .headers()
                .get("Access-Control-Allow-Headers")
                .unwrap()
                .to_str()
                .unwrap()
                .split(',')
                .map(|header| header.trim().to_ascii_lowercase())
                .collect::<Vec<_>>();
            allowed_headers.sort_unstable();
            assert_eq!(
                allowed_headers,
                [
                    "accept",
                    "authorization",
                    "content-type",
                    "opentubex-client-version",
                    "x-pairing-token",
                ]
            );
            assert_eq!(
                response.headers().get("Access-Control-Max-Age").unwrap(),
                "3600"
            );
            assert!(
                !response
                    .headers()
                    .contains_key("Access-Control-Allow-Credentials")
            );
            assert!(test::read_body(response).await.is_empty());
        }
    }
}

#[actix_web::test]
async fn cors_rejects_unsupported_preflights_and_preserves_plain_options() {
    let app = test::init_service(
        App::new()
            .service(HealthHandler::get_service())
            .wrap(actix_web::middleware::from_fn(crate::cors::cors_middleware)),
    )
    .await;
    for (method, headers, expected) in [
        (Some("GET"), None, StatusCode::NO_CONTENT),
        (Some("HEAD"), Some("Accept"), StatusCode::NO_CONTENT),
        (
            Some("GET"),
            Some("OpenTubeX-Client-Version, AUTHORIZATION"),
            StatusCode::NO_CONTENT,
        ),
        (None, None, StatusCode::BAD_REQUEST),
        (Some("TRACE"), None, StatusCode::BAD_REQUEST),
        (Some("get"), None, StatusCode::BAD_REQUEST),
        (Some("GET"), Some("x-unsupported"), StatusCode::BAD_REQUEST),
        (
            Some("GET"),
            Some("authorization,x-unsupported"),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut request = test::TestRequest::default()
            .method(Method::OPTIONS)
            .uri("/health")
            .insert_header(("Origin", "https://client.example"));
        if let Some(method) = method {
            request = request.insert_header(("Access-Control-Request-Method", method));
        }
        if let Some(headers) = headers {
            request = request.insert_header(("Access-Control-Request-Headers", headers));
        }
        let response = test::call_service(&app, request.to_request()).await;
        assert_eq!(response.status(), expected, "{method:?}, {headers:?}");
        if expected == StatusCode::BAD_REQUEST {
            assert!(
                !response
                    .headers()
                    .contains_key("Access-Control-Allow-Origin")
            );
        }
    }
    let request = test::TestRequest::default()
        .method(Method::OPTIONS)
        .uri("/health")
        .to_request();
    assert_eq!(
        test::call_service(&app, request).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[actix_web::test]
async fn cors_headers_cover_successes_and_api_errors_without_bypassing_auth() {
    let pool = pool().await;
    let token = seed_account(&pool).await;
    let limiter = web::Data::new(RateLimiter::default());
    let peer: std::net::SocketAddr = "192.0.2.1:1234".parse().unwrap();
    for _ in 0..crate::rate_limit::MAX_REQUESTS_PER_WINDOW {
        assert!(limiter.check(peer.ip()));
    }
    let (app, _) = App::new()
        .into_utoipa_app()
        .app_data(web::Data::new(pool))
        .app_data(limiter)
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(
        app.service(HealthHandler::get_service())
            .wrap(actix_web::middleware::from_fn(crate::cors::cors_middleware)),
    )
    .await;
    for (uri, authorization, expected) in [
        ("/health", None, StatusCode::OK),
        (
            "/v1/encrypted_sync/settings",
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/encrypted_sync/settings",
            Some("invalid-token"),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/v1/encrypted_sync/settings",
            Some(token.as_str()),
            StatusCode::OK,
        ),
        (
            "/encrypted_sync/settings",
            Some(token.as_str()),
            StatusCode::OK,
        ),
        ("/not-an-endpoint", None, StatusCode::NOT_FOUND),
    ] {
        let mut request = test::TestRequest::get()
            .uri(uri)
            .insert_header(("Origin", "https://client.example"))
            .insert_header(("OpenTubeX-Client-Version", "0.36.0"));
        if let Some(authorization) = authorization {
            request = request.insert_header(("Authorization", authorization));
        }
        let response = test::call_service(&app, request.to_request()).await;
        assert_eq!(response.status(), expected, "{uri}");
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Origin")
                .unwrap(),
            "*"
        );
        assert!(
            !response
                .headers()
                .contains_key("Access-Control-Allow-Credentials")
        );
    }
    let request = test::TestRequest::post()
        .uri("/account/login")
        .peer_addr(peer)
        .insert_header(("Origin", "https://client.example"))
        .set_json(json!({ "name": "test", "password": "wrong-password" }))
        .to_request();
    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Origin")
            .unwrap(),
        "*"
    );
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Expose-Headers")
            .unwrap(),
        "Allow, Retry-After"
    );
}

#[actix_web::test]
async fn removed_playback_speed_routes_return_not_found() {
    let pool = pool().await;
    let token = seed_account(&pool).await;
    let (app, _) = App::new()
        .into_utoipa_app()
        .app_data(web::Data::new(pool))
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(app).await;

    for prefix in ["/v1", ""] {
        for (method, suffix) in [
            (Method::GET, "/"),
            (Method::PUT, "/"),
            (Method::DELETE, "/channel-id"),
        ] {
            let uri = format!("{prefix}/channel_playback_speeds{suffix}");
            for authorization in [None, Some("invalid-token"), Some(token.as_str())] {
                let mut request = test::TestRequest::default()
                    .method(method.clone())
                    .uri(&uri)
                    .set_json(json!({ "channel_id": "channel-id", "playback_speed": 1.5 }));
                if let Some(authorization) = authorization {
                    request = request.insert_header(("Authorization", authorization));
                }
                let response =
                    response_snapshot(test::try_call_service(&app, request.to_request()).await)
                        .await;
                assert_eq!(response.0, StatusCode::NOT_FOUND, "{method} {uri}");
            }
        }
    }
}

#[actix_web::test]
async fn removed_playback_speed_routes_are_absent_from_openapi() {
    let (_, api) = App::new()
        .into_utoipa_app()
        .configure(configure_api_routes)
        .split_for_parts();
    assert!(
        api.paths
            .paths
            .keys()
            .all(|path| !path.contains("channel_playback_speeds"))
    );
}

#[actix_web::test]
async fn playback_speeds_are_read_only_while_settings_remain_writable() {
    for prefix in ["/v1", ""] {
        for existing in [false, true] {
            let pool = pool().await;
            let token = seed_account(&pool).await;
            if existing {
                crate::database::encrypted_sync::create(
                    &mut pool.get().await.unwrap(),
                    &crate::models::EncryptedSync {
                        account_id: "account".into(),
                        collection: "playbackSpeeds".into(),
                        revision: 1,
                        payload: "original-speeds-ciphertext".into(),
                    },
                )
                .await
                .unwrap();
            }
            let (app, _) = App::new()
                .into_utoipa_app()
                .app_data(web::Data::new(pool))
                .configure(configure_api_routes)
                .split_for_parts();
            let app = test::init_service(app).await;
            let revision = i64::from(existing);
            let uri = format!("{prefix}/encrypted_sync/playbackSpeeds");
            let request = test::TestRequest::put()
                .uri(&uri)
                .insert_header(("Authorization", token.clone()))
                .set_json(json!({ "revision": revision, "payload": "new-speeds-ciphertext" }))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{uri}, existing={existing}"
            );
            assert_eq!(response.headers().get("Allow").unwrap(), "GET");
            let request = test::TestRequest::get()
                .uri(&uri)
                .insert_header(("Authorization", token.clone()))
                .to_request();
            let result: Value = test::call_and_read_body_json(&app, request).await;
            assert_eq!(result["revision"], revision);
            assert_eq!(
                result["payload"],
                if existing {
                    json!("original-speeds-ciphertext")
                } else {
                    Value::Null
                }
            );
            let request = test::TestRequest::put()
                .uri(&format!("{prefix}/encrypted_sync/settings"))
                .insert_header(("Authorization", token))
                .set_json(json!({ "revision": 0, "payload": "settings-ciphertext" }))
                .to_request();
            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::OK
            );
        }
    }
}

#[actix_web::test]
async fn post_marks_round_trip_independently_of_older_video_clients() {
    let pool = pool().await;
    let token = seed_account(&pool).await;
    let (app, _) = App::new()
        .into_utoipa_app()
        .app_data(web::Data::new(pool))
        .app_data(web::Data::new(RateLimiter::default()))
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(app).await;
    for (collection, payload) in [
        ("seenPosts", "encrypted-post-marks"),
        ("seenVideos", "older-client-video-marks"),
    ] {
        let request = test::TestRequest::put()
            .uri(&format!("/v1/encrypted_sync/{collection}"))
            .insert_header(("Authorization", token.clone()))
            .set_json(json!({ "revision": 0, "payload": payload }))
            .to_request();
        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::OK
        );
    }
    let stale = test::TestRequest::put()
        .uri("/v1/encrypted_sync/seenPosts")
        .insert_header(("Authorization", token.clone()))
        .set_json(json!({ "revision": 0, "payload": "stale-post-marks" }))
        .to_request();
    assert_eq!(
        test::call_service(&app, stale).await.status(),
        StatusCode::CONFLICT
    );
    let request = test::TestRequest::get()
        .uri("/v1/encrypted_sync/seenPosts")
        .insert_header(("Authorization", token))
        .to_request();
    let result: Value = test::call_and_read_body_json(&app, request).await;
    assert_eq!(result["revision"], 1);
    assert_eq!(result["payload"], "encrypted-post-marks");
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
    for (method, path) in [(Method::GET, "sessions"), (Method::DELETE, "delete")] {
        let mut failures = Vec::new();
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
        assert_eq!(failures[0], failures[1], "{method} /account/{path}");
    }
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
    for uri in ["/", "/health", "/healthz", "/docs", "/meta"] {
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

#[actix_web::test]
async fn metadata_is_public_and_reports_version_and_oidc_configuration() {
    let (app, _) = App::new()
        .into_utoipa_app()
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(app.service(HealthHandler::get_service())).await;
    let request = test::TestRequest::get().uri("/meta").to_request();
    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = test::read_body_json(response).await;
    assert_eq!(
        body,
        json!({
            "api": { "libretube-sync": env!("CARGO_PKG_VERSION") },
            "version": env!("CARGO_PKG_VERSION"),
            "oidc": CONFIG.oidc.is_some(),
        })
    );
}

#[actix_web::test]
async fn upstream_query_parameters_are_documented() {
    let (_, api) = App::new()
        .into_utoipa_app()
        .configure(configure_api_routes)
        .split_for_parts();
    let api = serde_json::to_value(api).unwrap();
    let parameters = api["paths"]["/v1/watch_history/"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let names: Vec<_> = parameters
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["page", "page_size", "state", "order"]);
    assert!(parameters.iter().all(|p| p["in"] == "query"));
    let history = &api["paths"]["/v1/watch_history/"]["get"]["parameters"];
    assert!(
        history
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["required"] == false)
    );
}

#[actix_web::test]
async fn history_pagination_defaults_page_and_preserves_fork_limits() {
    let pool = pool().await;
    let token = seed_account(&pool).await;
    pool.get().await.unwrap().spawn_blocking(|conn| {
        use diesel::connection::SimpleConnection;
        conn.batch_execute(
            "INSERT INTO channel (id, name, verified) VALUES ('channel', 'Channel', FALSE);
             WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM numbers WHERE n < 1001)
             INSERT INTO video (id, title, upload_date, thumbnail_url, duration, uploader_id)
               SELECT 'video-' || n, 'Video', n, 'https://example.test/thumbnail', 60, 'channel' FROM numbers;
             INSERT INTO watch_history (video_id, account_id, added_date, watched_state)
               SELECT id, 'account', upload_date, 'watching' FROM video;",
        )?;
        Ok(())
    }).await.unwrap();
    let (app, _) = App::new()
        .into_utoipa_app()
        .app_data(web::Data::new(pool))
        .configure(configure_api_routes)
        .split_for_parts();
    let app = test::init_service(app).await;
    for prefix in ["/v1", ""] {
        for (query, count, first) in [
            ("", 50, Some("video-1001")),
            ("?page_size=100", 100, Some("video-1001")),
            ("?page=2&page_size=100", 100, Some("video-901")),
            ("?page=0&page_size=0", 1, Some("video-1001")),
            ("?page_size=4294967295", 1000, Some("video-1001")),
            ("?page=2&page_size=1000", 1, Some("video-1")),
            ("?page=4294967295&page_size=1000", 0, None),
        ] {
            let uri = format!("{prefix}/watch_history/{query}");
            let request = test::TestRequest::get()
                .uri(&uri)
                .insert_header(("Authorization", token.clone()))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let body: Vec<Value> = test::read_body_json(response).await;
            assert_eq!(body.len(), count, "{uri}");
            assert_eq!(
                body.first().and_then(|item| item["video"]["id"].as_str()),
                first,
                "{uri}"
            );
        }
    }
}
