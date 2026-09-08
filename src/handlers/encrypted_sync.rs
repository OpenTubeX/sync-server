use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::{HttpResponse, Responder, get, put, web};
use utoipa_actix_web::scope;

use crate::database::encrypted_sync;
use crate::database::watch_history::MAX_PAGE_SIZE;
use crate::dto::{
    EncryptedSyncCollectionResponse, EncryptedSyncCollectionRevision, EncryptedSyncManifest,
    PutEncryptedSync, SyncCapabilities,
};
use crate::handlers::user::{PlaintextSyncExempt, auth_middleware};
use crate::handlers::{HandlerError, HandlerResult, ScopedHandler};
use crate::models::Account;
use crate::{WebData, get_db_conn};

const MEBIBYTE: usize = 1024 * 1024;
const MAX_ENCRYPTED_SYNC_BYTES: usize = 64 * MEBIBYTE;
const MAX_ENCRYPTED_SYNC_ACCOUNT_BYTES: usize = 128 * MEBIBYTE;
// Current clients can acknowledge saved playback speeds in settings. Older
// clients still require the deprecated collection to resume partial migrations.
const LEGACY_ENCRYPTED_COLLECTIONS: [&str; 5] = [
    "subscriptions",
    "playlists",
    "history",
    "profiles",
    "playlistBookmarks",
];

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct EncryptedSyncManifestQuery {
    /// The requesting client has successfully synced playback speeds into settings.
    #[serde(default)]
    playback_speeds_in_settings: bool,
}

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
        scope::scope("/encrypted_sync")
            .app_data(web::JsonConfig::default().limit(MAX_ENCRYPTED_SYNC_BYTES + 1024))
            .app_data(web::Data::new(PlaintextSyncExempt))
            .wrap(actix_web::middleware::from_fn(auth_middleware))
            .service(get_encrypted_sync_manifest)
            .service(get_legacy_encrypted_sync)
            .service(get_encrypted_sync_collection)
            .service(put_encrypted_sync_collection)
    }
}

pub(crate) fn sync_capabilities() -> SyncCapabilities {
    SyncCapabilities {
        encrypted_sync: 1,
        bulk_sync: 1,
        history_page_size: MAX_PAGE_SIZE,
        key_pairing: 1,
        account_sessions: 1,
        seen_videos: 1,
    }
}

fn collection_limit(collection: &str) -> HandlerResult<usize> {
    match collection {
        "settings" => Ok(2 * MEBIBYTE),
        // Deprecated compatibility collection. Saved channel preferences now
        // belong in `settings`; keep accepting this while old clients remain.
        "sessions" | "sessionsV2" | "profiles" | "playbackSpeeds" => Ok(8 * MEBIBYTE),
        "subscriptions" | "playlistBookmarks" | "seenVideos" => Ok(16 * MEBIBYTE),
        "playlists" | "history" => Ok(MAX_ENCRYPTED_SYNC_BYTES),
        _ => Err(HandlerError::ValidationErrorWithContext(
            "unknown encrypted sync collection".to_owned(),
        )),
    }
}

#[utoipa::path(params(EncryptedSyncManifestQuery), responses((status = OK, body = EncryptedSyncManifest)), security(("api_jwt_token" = [])))]
#[get("")]
async fn get_encrypted_sync_manifest(
    account: Account,
    pool: WebData,
    query: web::Query<EncryptedSyncManifestQuery>,
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
    let has_migrated_playback_speeds = query.playback_speeds_in_settings
        || documents
            .iter()
            .any(|document| document.collection == "playbackSpeeds");
    let legacy_encrypted_data = !(has_all_migrated_collections && has_migrated_playback_speeds)
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
    collection_limit(&collection)?;
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
    if form.payload.len() > collection_limit(&collection)? {
        return Err(HandlerError::EncryptedSyncTooLarge);
    }

    let mut conn = get_db_conn!(pool);
    let next_revision = form.revision + 1;
    match encrypted_sync::save(
        &mut conn,
        &account.id,
        &collection,
        form.revision,
        &form.payload,
        MAX_ENCRYPTED_SYNC_ACCOUNT_BYTES,
    )
    .await
    .map_err(|_| HandlerError::InternalDatabaseError)?
    {
        encrypted_sync::SaveResult::Saved => {}
        encrypted_sync::SaveResult::Conflict => {
            return Err(HandlerError::EncryptedSyncConflict);
        }
        encrypted_sync::SaveResult::QuotaExceeded => {
            return Err(HandlerError::EncryptedSyncQuotaExceeded);
        }
    }

    Ok(HttpResponse::Ok().json(EncryptedSyncCollectionResponse {
        collection,
        revision: next_revision,
        payload: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::{MEBIBYTE, collection_limit};

    #[test]
    fn encrypted_collection_limits_are_scoped_by_data_type() {
        assert_eq!(collection_limit("settings").unwrap(), 2 * MEBIBYTE);
        assert_eq!(collection_limit("profiles").unwrap(), 8 * MEBIBYTE);
        assert_eq!(collection_limit("playbackSpeeds").unwrap(), 8 * MEBIBYTE);
        assert_eq!(collection_limit("sessions").unwrap(), 8 * MEBIBYTE);
        assert_eq!(collection_limit("sessionsV2").unwrap(), 8 * MEBIBYTE);
        assert_eq!(collection_limit("subscriptions").unwrap(), 16 * MEBIBYTE);
        assert_eq!(collection_limit("history").unwrap(), 64 * MEBIBYTE);
        assert_eq!(collection_limit("seenVideos").unwrap(), 16 * MEBIBYTE);
        assert_eq!(super::sync_capabilities().seen_videos, 1);
        assert!(collection_limit("unknown").is_err());
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod migration_tests {
    use actix_web::{App, HttpMessage, test, web};
    use diesel::connection::SimpleConnection;
    use diesel_async::RunQueryDsl;
    use diesel_async::pooled_connection::{AsyncDieselConnectionManager, bb8::Pool};
    use diesel_migrations::MigrationHarness;

    use crate::{DbConnection, MIGRATIONS, models::Account};

    #[actix_rt::test]
    async fn deleting_migrated_playback_speeds_does_not_restart_legacy_migration() {
        let pool = Pool::builder()
            .max_size(1)
            .build(AsyncDieselConnectionManager::<DbConnection>::new(
                ":memory:",
            ))
            .await
            .unwrap();
        let account = Account {
            id: "owner".into(),
            name_hash: "owner-hash".into(),
            password_hash: None,
            oidc_sub: None,
            legacy_tokens_enabled: false,
            session_generation: 0,
        };
        {
            let mut conn = pool.get().await.unwrap();
            conn.spawn_blocking(|conn| {
                conn.run_pending_migrations(MIGRATIONS).unwrap();
                conn.batch_execute(
                    "INSERT INTO account (id, name_hash) VALUES ('owner', 'owner-hash');
                     INSERT INTO encrypted_sync_single_document (account_id, revision, payload)
                       VALUES ('owner', 1, 'legacy-ciphertext');",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        }
        let app = test::init_service(
            App::new().app_data(web::Data::new(pool.clone())).service(
                web::scope("/sync")
                    .service(super::get_encrypted_sync_manifest)
                    .service(super::get_legacy_encrypted_sync)
                    .service(super::get_encrypted_sync_collection)
                    .service(super::put_encrypted_sync_collection),
            ),
        )
        .await;
        // Incomplete migrations must still expose the original document.
        let request = test::TestRequest::get().uri("/sync").to_request();
        request.extensions_mut().insert(account.clone());
        let manifest: serde_json::Value = test::call_and_read_body_json(&app, request).await;
        assert_eq!(manifest["legacy_encrypted_data"], true);

        // Use the actual PUT endpoint, including deprecated collection support.
        for collection in [
            "subscriptions",
            "playlists",
            "history",
            "profiles",
            "playlistBookmarks",
            "settings",
            "playbackSpeeds",
        ] {
            let request = test::TestRequest::put()
                .uri(&format!("/sync/{collection}"))
                .set_json(serde_json::json!({ "revision": 0, "payload": "ciphertext" }))
                .to_request();
            request.extensions_mut().insert(account.clone());
            assert!(
                test::call_service(&app, request)
                    .await
                    .status()
                    .is_success()
            );
            // Even an existing settings ciphertext cannot prove that speeds
            // were migrated: the user may have excluded that setting.
            let request = test::TestRequest::get().uri("/sync").to_request();
            request.extensions_mut().insert(account.clone());
            let manifest: serde_json::Value = test::call_and_read_body_json(&app, request).await;
            assert_eq!(
                manifest["legacy_encrypted_data"],
                collection != "playbackSpeeds"
            );
        }
        for deleted in [false, true] {
            if deleted {
                let mut conn = pool.get().await.unwrap();
                diesel::sql_query("DELETE FROM encrypted_sync WHERE account_id = 'owner' AND collection = 'playbackSpeeds'")
                    .execute(&mut conn).await.unwrap();
            }
            let request = test::TestRequest::get().uri("/sync").to_request();
            request.extensions_mut().insert(account.clone());
            let manifest: serde_json::Value = test::call_and_read_body_json(&app, request).await;
            assert_eq!(manifest["legacy_data"], false);
            assert_eq!(manifest["legacy_encrypted_data"], deleted);
            let request = test::TestRequest::get()
                .uri("/sync?playback_speeds_in_settings=true")
                .to_request();
            request.extensions_mut().insert(account.clone());
            let manifest: serde_json::Value = test::call_and_read_body_json(&app, request).await;
            assert_eq!(manifest["legacy_encrypted_data"], false);
            assert_eq!(
                manifest["collections"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["collection"] == "playbackSpeeds"),
                !deleted
            );
        }
        let request = test::TestRequest::get()
            .uri("/sync/playbackSpeeds")
            .to_request();
        request.extensions_mut().insert(account.clone());
        let collection: serde_json::Value = test::call_and_read_body_json(&app, request).await;
        assert_eq!(collection["revision"], 0);
        assert!(collection["payload"].is_null());

        // The legacy document stays readable for older clients.
        let request = test::TestRequest::get().uri("/sync/legacy").to_request();
        request.extensions_mut().insert(account);
        let legacy: serde_json::Value = test::call_and_read_body_json(&app, request).await;
        assert_eq!(legacy["payload"], "legacy-ciphertext");
    }
}
