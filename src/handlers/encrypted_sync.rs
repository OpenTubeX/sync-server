use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::{HttpRequest, HttpResponse, Responder, delete, get, post, put, web};
use utoipa_actix_web::scope;

use crate::database::encrypted_sync;
use crate::database::sync_event;
use crate::database::watch_history::MAX_PAGE_SIZE;
use crate::dto::{
    EncryptedSyncCollectionResponse, EncryptedSyncCollectionRevision, EncryptedSyncManifest,
    PutEncryptedSync, SyncCapabilities,
};
use crate::handlers::session::now_ms;
use crate::handlers::user::authenticate_session;
use crate::handlers::user::{PlaintextSyncExempt, auth_middleware};
use crate::handlers::{HandlerError, HandlerResult, ScopedHandler};
use crate::models::{Account, AccountSession};
use crate::sync_notifications::{ChangeWaiter, notify};
use crate::{WebData, get_db_conn};
use diesel_async::AsyncConnection;

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
            .app_data(
                web::JsonConfig::default()
                    .limit(MAX_ENCRYPTED_SYNC_BYTES + sync_event::MAX_EVENT_BYTES + 1024),
            )
            .app_data(web::Data::new(PlaintextSyncExempt))
            .wrap(actix_web::middleware::from_fn(auth_middleware))
            .service(get_encrypted_sync_manifest)
            .service(get_sync_changes)
            .service(get_sync_events)
            .service(send_device_request)
            .service(acknowledge_device_request)
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
        seen_posts: 1,
        live_sync: 1,
    }
}

fn collection_limit(collection: &str) -> HandlerResult<usize> {
    match collection {
        "settings" => Ok(2 * MEBIBYTE),
        // Deprecated compatibility collection. Saved channel preferences now
        // belong in `settings`; keep accepting this while old clients remain.
        "sessions" | "sessionsV2" | "profiles" | "playbackSpeeds" => Ok(8 * MEBIBYTE),
        "subscriptions" | "playlistBookmarks" | "seenVideos" | "seenPosts" => Ok(16 * MEBIBYTE),
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
    let documents = encrypted_sync::revisions(&mut conn, &account.id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    let legacy_data = encrypted_sync::has_legacy_data(&mut conn, &account.id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    let has_all_migrated_collections = LEGACY_ENCRYPTED_COLLECTIONS
        .iter()
        .all(|collection| documents.iter().any(|(name, _)| name == collection));
    let has_migrated_playback_speeds = query.playback_speeds_in_settings
        || documents.iter().any(|(name, _)| name == "playbackSpeeds");
    let legacy_encrypted_data = !(has_all_migrated_collections && has_migrated_playback_speeds)
        && encrypted_sync::get_legacy_encrypted(&mut conn, &account.id)
            .await
            .map_err(|_| HandlerError::InternalDatabaseError)?
            .is_some();

    Ok(web::Json(EncryptedSyncManifest {
        collections: documents
            .into_iter()
            .map(|(collection, revision)| EncryptedSyncCollectionRevision {
                collection,
                revision,
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
    let next_revision = form
        .revision
        .checked_add(1)
        .filter(|_| form.revision >= 0)
        .ok_or(HandlerError::ValidationError)?;
    if form
        .activity
        .as_ref()
        .is_some_and(|payload| payload.len() > sync_event::MAX_EVENT_BYTES)
    {
        return Err(HandlerError::EncryptedSyncTooLarge);
    }
    if form
        .activity
        .as_ref()
        .is_some_and(|payload| payload.is_empty())
    {
        return Err(HandlerError::ValidationError);
    }
    let now = now_ms()?;
    match encrypted_sync::save(
        &mut conn,
        &account.id,
        &collection,
        form.revision,
        &form.payload,
        MAX_ENCRYPTED_SYNC_ACCOUNT_BYTES,
        form.activity.as_deref().map(|payload| (payload, now)),
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

    notify(&account.id, None);
    Ok(HttpResponse::Ok().json(EncryptedSyncCollectionResponse {
        collection,
        revision: next_revision,
        payload: None,
    }))
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct ChangesQuery {
    /// Opaque cursor from the previous /changes response.
    #[serde(default)]
    since: String,
}

async fn change_cursor(pool: &WebData, owner: &str, device: &str) -> HandlerResult<String> {
    let mut conn = get_db_conn!(pool);
    let revisions = encrypted_sync::revisions(&mut conn, owner)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    let events = sync_event::event_ids(&mut conn, owner, device, now_ms()?)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let bytes =
        serde_json::to_vec(&(revisions, events)).map_err(|_| HandlerError::ValidationError)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}

#[utoipa::path(params(ChangesQuery), security(("api_jwt_token" = [])))]
#[get("/changes")]
async fn get_sync_changes(
    req: HttpRequest,
    account: Account,
    session: AccountSession,
    pool: WebData,
    query: web::Query<ChangesQuery>,
) -> HandlerResult<impl Responder> {
    // Register before reading the durable cursor so concurrent commits cannot be missed.
    let waiter = ChangeWaiter::new(&account.id, &session.device_id);
    let mut cursor = change_cursor(&pool, &account.id, &session.device_id).await?;
    if cursor == query.since {
        let _ = actix_web::rt::time::timeout(std::time::Duration::from_secs(25), waiter).await;
        // Recheck revocation/expiry after the wait and never hold a DB connection while idle.
        authenticate_session(&req, &pool).await?;
        cursor = change_cursor(&pool, &account.id, &session.device_id).await?;
    }
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(serde_json::json!({ "cursor": cursor })))
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct EventsQuery {
    /// Last processed broadcast event ID, not the opaque /changes cursor.
    #[serde(default)]
    since: String,
}

#[utoipa::path(params(EventsQuery), security(("api_jwt_token" = [])))]
#[get("/events")]
async fn get_sync_events(
    account: Account,
    session: AccountSession,
    pool: WebData,
    query: web::Query<EventsQuery>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);
    let events = sync_event::list(&mut conn, &account.id, &session.device_id, now_ms()?)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    let events: Vec<_> = events
        .into_iter()
        .filter(|event| !event.recipient.is_empty() || event.id > query.since)
        .collect();
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(events))
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct DeviceRequest {
    recipient: String,
    payload: String,
}

#[utoipa::path(request_body = DeviceRequest, security(("api_jwt_token" = [])))]
#[post("/events")]
async fn send_device_request(
    account: Account,
    session: AccountSession,
    pool: WebData,
    form: web::Json<DeviceRequest>,
) -> HandlerResult<impl Responder> {
    if form.payload.is_empty()
        || form.payload.len() > sync_event::MAX_EVENT_BYTES
        || form.recipient == session.device_id
    {
        return Err(HandlerError::ValidationError);
    }
    let now = now_ms()?;
    let mut conn = get_db_conn!(pool);
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        Box::pin(async {
            // Serialize recipient validation and queue updates with session revocation.
            use diesel::prelude::*;
            use diesel_async::RunQueryDsl;
            let locked =
                diesel::update(crate::schema::account::table.find(&account.id).filter(
                    crate::schema::account::session_generation.eq(account.session_generation),
                ))
                .set(crate::schema::account::id.eq(&account.id))
                .execute(conn)
                .await?;
            if locked != 1 {
                return Err(diesel::result::Error::NotFound);
            }
            let sessions = crate::database::account_session::list_active(
                conn,
                &account.id,
                account.session_generation,
                now_ms().map_err(|_| diesel::result::Error::RollbackTransaction)?,
            )
            .await?;
            if !sessions
                .iter()
                .any(|target| target.device_id == form.recipient)
            {
                return Err(diesel::result::Error::NotFound);
            }
            sync_event::append(conn, &account.id, &form.recipient, &form.payload, now).await
        })
    })
    .await
    .map_err(|error| match error {
        diesel::result::Error::NotFound => HandlerError::AccountSessionNotFound,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::CheckViolation,
            _,
        ) => HandlerError::EncryptedSyncQuotaExceeded,
        _ => HandlerError::InternalDatabaseError,
    })?;
    notify(&account.id, Some(&form.recipient));
    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(security(("api_jwt_token" = [])))]
#[delete("/events/{id}")]
async fn acknowledge_device_request(
    account: Account,
    session: AccountSession,
    pool: WebData,
    event: web::Path<String>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);
    sync_event::acknowledge(&mut conn, &account.id, &session.device_id, &event)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    notify(&account.id, Some(&session.device_id));
    Ok(HttpResponse::NoContent().finish())
}

#[cfg(test)]
mod tests {
    use super::{MEBIBYTE, collection_limit};

    #[test]
    fn seen_posts_are_a_separate_advertised_encrypted_collection() {
        assert_eq!(collection_limit("seenPosts").unwrap(), 16 * MEBIBYTE);
        let capabilities = serde_json::to_value(super::sync_capabilities()).unwrap();
        assert_eq!(capabilities["seen_posts"], 1);
    }

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
    async fn old_and_new_clients_can_alternate_collection_writes_without_losing_activity() {
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
                    "INSERT INTO account (id, name_hash) VALUES ('owner', 'owner-hash');",
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
                    .service(super::get_encrypted_sync_collection)
                    .service(super::put_encrypted_sync_collection),
            ),
        )
        .await;
        let request = test::TestRequest::put()
            .uri("/sync/settings")
            .set_json(serde_json::json!({"revision": 0, "payload": "ciphertext", "activity": ""}))
            .to_request();
        request.extensions_mut().insert(account.clone());
        assert_eq!(
            test::call_service(&app, request).await.status().as_u16(),
            400
        );
        for (revision, payload, activity) in [
            (0, "old-client-first", None),
            (1, "new-client", Some("encrypted-activity")),
            (2, "old-client-again", None),
        ] {
            let mut body = serde_json::json!({ "revision": revision, "payload": payload });
            if let Some(activity) = activity {
                body["activity"] = activity.into();
            }
            let request = test::TestRequest::put()
                .uri("/sync/settings")
                .set_json(body)
                .to_request();
            request.extensions_mut().insert(account.clone());
            let response: serde_json::Value = test::call_and_read_body_json(&app, request).await;
            assert_eq!(
                response,
                serde_json::json!({
                    "collection": "settings", "revision": revision + 1, "payload": null
                })
            );

            let request = test::TestRequest::get().uri("/sync/settings").to_request();
            request.extensions_mut().insert(account.clone());
            let response: serde_json::Value = test::call_and_read_body_json(&app, request).await;
            assert_eq!(
                response,
                serde_json::json!({
                    "collection": "settings", "revision": revision + 1, "payload": payload
                })
            );

            let request = test::TestRequest::get().uri("/sync").to_request();
            request.extensions_mut().insert(account.clone());
            let manifest: serde_json::Value = test::call_and_read_body_json(&app, request).await;
            assert_eq!(
                manifest["collections"],
                serde_json::json!([
                    { "collection": "settings", "revision": revision + 1 }
                ])
            );
            assert_eq!(manifest["legacy_data"], false);
            assert_eq!(manifest["legacy_encrypted_data"], false);
            let mut conn = pool.get().await.unwrap();
            let events = crate::database::sync_event::list(
                &mut conn,
                &account.id,
                "",
                super::now_ms().unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(events.len(), usize::from(revision > 0));
            if revision > 0 {
                assert_eq!(events[0].payload, "encrypted-activity");
            }
        }
    }

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

#[cfg(all(test, feature = "sqlite"))]
mod live_tests {
    use super::*;
    use crate::{DbConnection, MIGRATIONS};
    use actix_web::{App, HttpMessage, test};
    use diesel::connection::SimpleConnection;
    use diesel_async::pooled_connection::{AsyncDieselConnectionManager, bb8::Pool};
    use diesel_migrations::MigrationHarness;

    #[actix_rt::test]
    async fn device_endpoints_restrict_delivery_and_acknowledgment_to_the_target_account_and_device()
     {
        let pool = Pool::builder()
            .max_size(1)
            .build(AsyncDieselConnectionManager::<DbConnection>::new(
                ":memory:",
            ))
            .await
            .unwrap();
        let now = now_ms().unwrap();
        let account = Account {
            id: "owner".into(),
            name_hash: "owner".into(),
            password_hash: None,
            oidc_sub: None,
            legacy_tokens_enabled: false,
            session_generation: 0,
        };
        let sender = AccountSession {
            id: "sender-session".into(),
            account_id: account.id.clone(),
            device_id: "sender".into(),
            encrypted_device_info: None,
            created_at: now,
            last_active_at: now,
            expires_at: now + 86400000,
            revoked_at: None,
            legacy: false,
            generation: 0,
            pending_pairing: false,
        };
        let recipient = AccountSession {
            id: "recipient-session".into(),
            device_id: "recipient".into(),
            ..sender.clone()
        };
        {
            let mut conn = pool.get().await.unwrap();
            conn.spawn_blocking(|conn| {
                conn.run_pending_migrations(MIGRATIONS).unwrap();
                conn.batch_execute("INSERT INTO account (id, name_hash) VALUES ('owner', 'owner'), ('other', 'other');")?;
                Ok(())
            }).await.unwrap();
            crate::database::account_session::create(&mut conn, &sender)
                .await
                .unwrap();
            crate::database::account_session::create(&mut conn, &recipient)
                .await
                .unwrap();
        }
        let app = test::init_service(
            App::new().app_data(web::Data::new(pool.clone())).service(
                web::scope("/sync")
                    .service(get_sync_changes)
                    .service(get_sync_events)
                    .service(send_device_request)
                    .service(acknowledge_device_request),
            ),
        )
        .await;
        let request = |builder: test::TestRequest, owner: &Account, session: &AccountSession| {
            let request = builder.to_request();
            request.extensions_mut().insert(owner.clone());
            request.extensions_mut().insert(session.clone());
            request
        };
        let before: serde_json::Value = test::call_and_read_body_json(
            &app,
            request(
                test::TestRequest::get().uri("/sync/changes"),
                &account,
                &recipient,
            ),
        )
        .await;
        for (target, status) in [("missing", 404), ("sender", 400), ("recipient", 204)] {
            let response = test::call_service(
                &app,
                request(
                    test::TestRequest::post().uri("/sync/events").set_json(
                        serde_json::json!({"recipient": target, "payload": "encrypted-video"}),
                    ),
                    &account,
                    &sender,
                ),
            )
            .await;
            assert_eq!(response.status().as_u16(), status);
        }
        let after: serde_json::Value = test::call_and_read_body_json(
            &app,
            request(
                test::TestRequest::get().uri("/sync/changes"),
                &account,
                &recipient,
            ),
        )
        .await;
        assert_ne!(before["cursor"], after["cursor"]);
        let messages: serde_json::Value = test::call_and_read_body_json(
            &app,
            request(
                test::TestRequest::get().uri("/sync/events"),
                &account,
                &recipient,
            ),
        )
        .await;
        assert_eq!(messages.as_array().unwrap().len(), 1);
        assert_eq!(messages[0]["payload"], "encrypted-video");
        let event_id = messages[0]["id"].as_str().unwrap();
        let other_account = Account {
            id: "other".into(),
            name_hash: "other".into(),
            ..account.clone()
        };
        for (owner, device) in [(&account, &sender), (&other_account, &recipient)] {
            let messages: serde_json::Value = test::call_and_read_body_json(
                &app,
                request(test::TestRequest::get().uri("/sync/events"), owner, device),
            )
            .await;
            assert!(messages.as_array().unwrap().is_empty());
            let response = test::call_service(
                &app,
                request(
                    test::TestRequest::delete().uri(&format!("/sync/events/{event_id}")),
                    owner,
                    device,
                ),
            )
            .await;
            assert_eq!(response.status().as_u16(), 204);
        }
        let messages: serde_json::Value = test::call_and_read_body_json(
            &app,
            request(
                test::TestRequest::get().uri("/sync/events"),
                &account,
                &recipient,
            ),
        )
        .await;
        assert_eq!(messages.as_array().unwrap().len(), 1);
        let response = test::call_service(
            &app,
            request(
                test::TestRequest::delete().uri(&format!("/sync/events/{event_id}")),
                &account,
                &recipient,
            ),
        )
        .await;
        assert_eq!(response.status().as_u16(), 204);
        let messages: serde_json::Value = test::call_and_read_body_json(
            &app,
            request(
                test::TestRequest::get().uri("/sync/events"),
                &account,
                &recipient,
            ),
        )
        .await;
        assert!(messages.as_array().unwrap().is_empty());

        // Simulate a revocation becoming visible as the send acquires its account lock.
        // Validation before that lock incorrectly accepts the now-revoked recipient.
        pool.get()
            .await
            .unwrap()
            .spawn_blocking(|conn| {
                conn.batch_execute(
                    "CREATE TRIGGER revoke_recipient_on_lock BEFORE UPDATE ON account
                WHEN NEW.id = 'owner' BEGIN
                UPDATE account_session SET revoked_at = 1 WHERE id = 'recipient-session';
                END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let response = test::call_service(
            &app,
            request(
                test::TestRequest::post().uri("/sync/events").set_json(
                    serde_json::json!({"recipient": "recipient", "payload": "must not be queued"}),
                ),
                &account,
                &sender,
            ),
        )
        .await;
        assert_eq!(response.status().as_u16(), 404);
        let messages: serde_json::Value = test::call_and_read_body_json(
            &app,
            request(
                test::TestRequest::get().uri("/sync/events"),
                &account,
                &recipient,
            ),
        )
        .await;
        assert!(messages.as_array().unwrap().is_empty());
    }
}
