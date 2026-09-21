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
        scope::scope("")
            .app_data(web::Data::new(crate::CONFIG.privacy_policy_url.clone()))
            .service(health_state)
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
                match &policy {
                    Some(url) => assert_eq!(json["privacy_policy_url"], *url),
                    None => assert!(json.get("privacy_policy_url").is_none()),
                }
            }
        }
    }
}
