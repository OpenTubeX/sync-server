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
    })
}
