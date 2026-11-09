use actix_web::{HttpResponse, Responder, get, routes, web};
use utoipa_actix_web::scope;

use crate::{
    CONFIG,
    config::OidcConfig,
    dto::{HealthResponse, MetaResponse, MetaResponseApi, MetaResponseCapabilities},
    handlers::{HandlerResult, ScopedHandler, encrypted_sync::sync_capabilities},
};

pub struct HealthHandler {}
impl ScopedHandler for HealthHandler {
    fn get_service() -> utoipa_actix_web::scope::Scope<
        impl actix_web::dev::ServiceFactory<
            actix_web::dev::ServiceRequest,
            Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        >,
    > {
        scope::scope("")
            .app_data(web::Data::new(crate::CONFIG.privacy_policy_url.clone()))
            .service(health_state)
            .service(server_metainfo)
    }
}

#[utoipa::path(responses((status = OK, body = HealthResponse)))]
#[routes]
#[get("/")]
#[get("/health")]
#[get("/healthz")]
async fn health_state(privacy_policy_url: web::Data<Option<String>>) -> impl Responder {
    web::Json(HealthResponse {
        status: "ok".to_owned(),
        capabilities: sync_capabilities(),
        privacy_policy_url: privacy_policy_url.get_ref().clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oidc_display_name_is_trimmed_and_omitted_when_blank() {
        let config = |display_name: Option<&str>| OidcConfig {
            provider_url: "https://auth.example.com".to_owned(),
            client_id: "sync-server".to_owned(),
            client_secret: "secret".to_owned(),
            app_url: "https://sync.example.com".to_owned(),
            allowed_redirect_urls: Vec::new(),
            display_name: display_name.map(str::to_owned),
        };
        assert_eq!(oidc_display_name(None), None);
        assert_eq!(oidc_display_name(Some(&config(None))), None);
        assert_eq!(oidc_display_name(Some(&config(Some("  ")))), None);
        assert_eq!(
            oidc_display_name(Some(&config(Some(" Authelia ")))),
            Some("Authelia".to_owned())
        );
    }

    #[actix_web::test]
    async fn health_routes_advertise_only_the_configured_privacy_policy() {
        for policy in [None, Some("https://operator.example/privacy".to_owned())] {
            let app = actix_web::test::init_service(
                actix_web::App::new()
                    .app_data(web::Data::new(policy.clone()))
                    .service(health_state),
            )
            .await;
            for path in ["/", "/health", "/healthz"] {
                let request = actix_web::test::TestRequest::get().uri(path).to_request();
                let json: serde_json::Value =
                    actix_web::test::call_and_read_body_json(&app, request).await;
                assert_eq!(json["status"], "ok");
                assert_eq!(json["capabilities"]["encrypted_sync"], 1);
                assert_eq!(json["capabilities"]["external_media_positions"], 1);
                assert_eq!(json["capabilities"]["seen_rss_items"], 1);
                assert_eq!(json["capabilities"]["notification_dismissals"], 1);
                match &policy {
                    Some(url) => assert_eq!(json["privacy_policy_url"], *url),
                    None => assert!(json.get("privacy_policy_url").is_none()),
                }
            }
        }
    }
}

fn oidc_display_name(oidc: Option<&OidcConfig>) -> Option<String> {
    oidc.and_then(|oidc| oidc.display_name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

#[utoipa::path(responses((status = OK, body = MetaResponse)))]
#[get("/meta")]
async fn server_metainfo() -> HandlerResult<impl Responder> {
    Ok(HttpResponse::Ok().json(MetaResponse {
        api: MetaResponseApi {
            base: env!("CARGO_PKG_VERSION").into(),
        },
        version: env!("CARGO_PKG_VERSION").into(),
        capabilities: MetaResponseCapabilities {
            oidc: CONFIG.oidc.is_some(),
            oidc_display_name: oidc_display_name(CONFIG.oidc.as_ref()),
        },
    }))
}
