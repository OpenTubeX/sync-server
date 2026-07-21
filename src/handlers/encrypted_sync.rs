use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::{HttpResponse, Responder, get, put, web};
use diesel::result::DatabaseErrorKind;
use utoipa_actix_web::scope;

use crate::database::encrypted_sync;
use crate::database::watch_history::MAX_PAGE_SIZE;
use crate::dto::{
    EncryptedSyncCollectionResponse, EncryptedSyncCollectionRevision, EncryptedSyncManifest,
    PutEncryptedSync, SyncCapabilities,
};
use crate::handlers::user::auth_middleware;
use crate::handlers::{HandlerError, HandlerResult, ScopedHandler};
use crate::models::{Account, EncryptedSync};
use crate::{WebData, get_db_conn};

const MAX_ENCRYPTED_SYNC_BYTES: usize = 64 * 1024 * 1024;
const COLLECTIONS: [&str; 8] = [
    "subscriptions",
    "playlists",
    "history",
    "playbackSpeeds",
    "profiles",
    "settings",
    "playlistBookmarks",
    "sessions",
];
const LEGACY_ENCRYPTED_COLLECTIONS: [&str; 6] = [
    "subscriptions",
    "playlists",
    "history",
    "playbackSpeeds",
    "profiles",
    "playlistBookmarks",
];

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
                .service(get_encrypted_sync_manifest)
                .service(get_legacy_encrypted_sync)
                .service(get_encrypted_sync_collection)
                .service(put_encrypted_sync_collection),
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

fn validate_collection(collection: &str) -> HandlerResult<()> {
    if COLLECTIONS.contains(&collection) {
        Ok(())
    } else {
        Err(HandlerError::ValidationErrorWithContext(
            "unknown encrypted sync collection".to_owned(),
        ))
    }
}

#[utoipa::path(responses((status = OK, body = EncryptedSyncManifest)), security(("api_jwt_token" = [])))]
#[get("")]
async fn get_encrypted_sync_manifest(
    account: Account,
    pool: WebData,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);
    let documents = encrypted_sync::get_all(&mut conn, &account.id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    let legacy_data = encrypted_sync::has_legacy_data(&mut conn, &account.id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    let has_all_migrated_collections = LEGACY_ENCRYPTED_COLLECTIONS.iter().all(|collection| {
        documents
            .iter()
            .any(|document| document.collection == *collection)
    });
    let legacy_encrypted_data = !has_all_migrated_collections
        && encrypted_sync::get_legacy_encrypted(&mut conn, &account.id)
            .await
            .map_err(|_| HandlerError::InternalDatabaseError)?
            .is_some();

    Ok(web::Json(EncryptedSyncManifest {
        collections: documents
            .into_iter()
            .map(|document| EncryptedSyncCollectionRevision {
                collection: document.collection,
                revision: document.revision,
            })
            .collect(),
        legacy_data,
        legacy_encrypted_data,
    }))
}

#[utoipa::path(responses((status = OK, body = EncryptedSyncCollectionResponse)), security(("api_jwt_token" = [])))]
#[get("/legacy")]
async fn get_legacy_encrypted_sync(
    account: Account,
    pool: WebData,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);
    let document = encrypted_sync::get_legacy_encrypted(&mut conn, &account.id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;

    Ok(web::Json(match document {
        Some(document) => EncryptedSyncCollectionResponse {
            collection: "legacy".to_owned(),
            revision: document.revision,
            payload: Some(document.payload),
        },
        None => EncryptedSyncCollectionResponse {
            collection: "legacy".to_owned(),
            revision: 0,
            payload: None,
        },
    }))
}

#[utoipa::path(responses((status = OK, body = EncryptedSyncCollectionResponse)), security(("api_jwt_token" = [])))]
#[get("/{collection}")]
async fn get_encrypted_sync_collection(
    account: Account,
    pool: WebData,
    collection: web::Path<String>,
) -> HandlerResult<impl Responder> {
    let collection = collection.into_inner();
    validate_collection(&collection)?;
    let mut conn = get_db_conn!(pool);
    let document = encrypted_sync::get(&mut conn, &account.id, &collection)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;

    Ok(web::Json(match document {
        Some(document) => EncryptedSyncCollectionResponse {
            collection,
            revision: document.revision,
            payload: Some(document.payload),
        },
        None => EncryptedSyncCollectionResponse {
            collection,
            revision: 0,
            payload: None,
        },
    }))
}

#[utoipa::path(request_body = PutEncryptedSync, responses((status = OK, body = EncryptedSyncCollectionResponse)), security(("api_jwt_token" = [])))]
#[put("/{collection}")]
async fn put_encrypted_sync_collection(
    account: Account,
    pool: WebData,
    collection: web::Path<String>,
    form: web::Json<PutEncryptedSync>,
) -> HandlerResult<impl Responder> {
    let collection = collection.into_inner();
    validate_collection(&collection)?;
    if form.payload.len() > MAX_ENCRYPTED_SYNC_BYTES {
        return Err(HandlerError::EncryptedSyncTooLarge);
    }

    let mut conn = get_db_conn!(pool);
    let next_revision = form.revision + 1;
    let saved = if form.revision == 0 {
        let document = EncryptedSync {
            account_id: account.id.clone(),
            collection: collection.clone(),
            revision: next_revision,
            payload: form.payload.clone(),
        };
        encrypted_sync::create_initial(&mut conn, &document)
            .await
            .map(|_| true)
            .or_else(|error| match error {
                diesel::result::Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => {
                    Ok(false)
                }
                error => Err(error),
            })
    } else {
        encrypted_sync::replace(
            &mut conn,
            &account.id,
            &collection,
            form.revision,
            &form.payload,
        )
        .await
    }
    .map_err(|_| HandlerError::InternalDatabaseError)?;

    if !saved {
        return Err(HandlerError::EncryptedSyncConflict);
    }

    Ok(HttpResponse::Ok().json(EncryptedSyncCollectionResponse {
        collection,
        revision: next_revision,
        payload: None,
    }))
}
