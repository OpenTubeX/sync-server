use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::{HttpResponse, Responder, get, put, web};
use diesel::result::DatabaseErrorKind;
use utoipa_actix_web::scope;

use crate::database::encrypted_sync;
use crate::database::watch_history::MAX_PAGE_SIZE;
use crate::dto::{EncryptedSyncResponse, PutEncryptedSync, SyncCapabilities};
use crate::handlers::user::auth_middleware;
use crate::handlers::{HandlerError, HandlerResult, ScopedHandler};
use crate::models::{Account, EncryptedSync};
use crate::{WebData, get_db_conn};

const MAX_ENCRYPTED_SYNC_BYTES: usize = 64 * 1024 * 1024;

pub struct EncryptedSyncHandler {}

impl ScopedHandler for EncryptedSyncHandler {
    fn get_service() -> scope::Scope<
        impl ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<impl MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        >,
    > {
        scope::scope("").service(
            scope::scope("/encrypted_sync")
                .app_data(web::JsonConfig::default().limit(MAX_ENCRYPTED_SYNC_BYTES + 1024))
                .wrap(actix_web::middleware::from_fn(auth_middleware))
                .service(get_encrypted_sync)
                .service(put_encrypted_sync),
        )
    }
}

pub(crate) fn sync_capabilities() -> SyncCapabilities {
    SyncCapabilities {
        encrypted_sync: 1,
        bulk_sync: 1,
        history_page_size: MAX_PAGE_SIZE,
    }
}

#[utoipa::path(responses((status = OK, body = EncryptedSyncResponse)), security(("api_jwt_token" = [])))]
#[get("")]
async fn get_encrypted_sync(account: Account, pool: WebData) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);
    let document = encrypted_sync::get(&mut conn, &account.id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    let legacy_data = encrypted_sync::has_legacy_data(&mut conn, &account.id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;

    Ok(web::Json(match document {
        Some(document) => EncryptedSyncResponse {
            revision: document.revision,
            payload: Some(document.payload),
            legacy_data,
        },
        None => EncryptedSyncResponse {
            revision: 0,
            payload: None,
            legacy_data,
        },
    }))
}

#[utoipa::path(request_body = PutEncryptedSync, responses((status = OK, body = EncryptedSyncResponse)), security(("api_jwt_token" = [])))]
#[put("")]
async fn put_encrypted_sync(
    account: Account,
    pool: WebData,
    form: web::Json<PutEncryptedSync>,
) -> HandlerResult<impl Responder> {
    if form.payload.len() > MAX_ENCRYPTED_SYNC_BYTES {
        return Err(HandlerError::EncryptedSyncTooLarge);
    }

    let mut conn = get_db_conn!(pool);
    let next_revision = form.revision + 1;
    let saved = if form.revision == 0 {
        let document = EncryptedSync {
            account_id: account.id.clone(),
            revision: next_revision,
            payload: form.payload.clone(),
        };
        encrypted_sync::create_and_clear_legacy(&mut conn, &document)
            .await
            .map(|_| true)
            .or_else(|error| match error {
                diesel::result::Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => {
                    Ok(false)
                }
                error => Err(error),
            })
    } else {
        encrypted_sync::replace(&mut conn, &account.id, form.revision, &form.payload).await
    }
    .map_err(|_| HandlerError::InternalDatabaseError)?;

    if !saved {
        return Err(HandlerError::EncryptedSyncConflict);
    }

    Ok(HttpResponse::Ok().json(EncryptedSyncResponse {
        revision: next_revision,
        payload: None,
        legacy_data: false,
    }))
}
