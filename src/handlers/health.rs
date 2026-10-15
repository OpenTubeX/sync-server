use actix_web::{HttpResponse, Responder, get, routes};
use utoipa_actix_web::scope;

use crate::{
    CONFIG,
    dto::{MetaResponse, MetaResponseApi, MetaResponseCapabilities},
    handlers::{HandlerResult, ScopedHandler},
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
            .service(health_state)
            .service(server_metainfo)
    }
}

#[utoipa::path(responses((status = OK, body = String)))]
#[routes]
#[get("/")]
#[get("/health")]
#[get("/healthz")]
async fn health_state() -> impl Responder {
    "OK"
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
        },
    }))
}
