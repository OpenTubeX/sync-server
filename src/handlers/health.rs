use actix_web::{Responder, routes, web};
use utoipa_actix_web::scope;

use crate::dto::HealthResponse;
use crate::handlers::{ScopedHandler, encrypted_sync::sync_capabilities};

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
        scope::scope("").service(health_state)
    }
}

#[utoipa::path(responses((status = OK, body = HealthResponse)))]
#[routes]
#[get("/")]
#[get("/health")]
#[get("/healthz")]
async fn health_state() -> impl Responder {
    web::Json(HealthResponse {
        status: "ok".to_owned(),
        capabilities: sync_capabilities(),
        privacy_policy_url: crate::CONFIG.privacy_policy_url.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[actix_web::test]
    async fn health_routes_return_the_configured_privacy_policy() {
        let app = actix_web::test::init_service(actix_web::App::new().service(health_state)).await;
        for path in ["/", "/health", "/healthz"] {
            let request = actix_web::test::TestRequest::get().uri(path).to_request();
            let response: HealthResponse =
                actix_web::test::call_and_read_body_json(&app, request).await;
            assert_eq!(response.status, "ok");
            assert_eq!(
                response.privacy_policy_url,
                crate::CONFIG.privacy_policy_url
            );
        }
    }

    #[test]
    fn health_advertises_only_configured_privacy_policy() {
        for policy in [None, Some("https://operator.example/privacy".to_owned())] {
            let response = HealthResponse {
                status: "ok".to_owned(),
                capabilities: sync_capabilities(),
                privacy_policy_url: policy.clone(),
            };
            let json = serde_json::to_value(response).unwrap();
            assert_eq!(json["status"], "ok");
            assert_eq!(json["capabilities"]["encrypted_sync"], 1);
            match policy {
                Some(url) => assert_eq!(json["privacy_policy_url"], url),
                None => assert!(json.get("privacy_policy_url").is_none()),
            }
        }
    }
}
