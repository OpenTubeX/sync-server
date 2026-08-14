#![allow(deprecated)]

use actix_web::{HttpResponse, Responder, delete, get, middleware::from_fn, put, web};
use diesel_async::{AsyncConnection, scoped_futures::ScopedFutureExt};
use utoipa_actix_web::scope;

use crate::{
    DbConnection, WebData,
    database::{
        channel_playback_speed::{
            get_channel_playback_speeds_by_account_id, remove_channel_playback_speed,
            set_channel_playback_speed,
        },
        quota::count_playback_speeds,
    },
    get_db_conn,
    handlers::{
        HandlerError, HandlerResult, ScopedHandler, check_stored_rows, user::auth_middleware,
    },
    models::{Account, ChannelPlaybackSpeed},
};

const DEPRECATION_HEADER: (&str, &str) = ("Deprecation", "@1786665600");

pub struct ChannelPlaybackSpeedsHandler;

impl ScopedHandler for ChannelPlaybackSpeedsHandler {
    fn get_service() -> utoipa_actix_web::scope::Scope<
        impl actix_web::dev::ServiceFactory<
            actix_web::dev::ServiceRequest,
            Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        >,
    > {
        scope("/channel_playback_speeds")
            .wrap(from_fn(auth_middleware))
            .service(get_channel_playback_speeds)
            .service(put_channel_playback_speed)
            .service(delete_channel_playback_speed)
    }
}

/// Get saved channel playback speeds through the deprecated dedicated API.
///
/// New OpenTubeX clients sync saved channel preferences through the encrypted
/// `settings` collection. This endpoint remains available for older clients.
#[deprecated(note = "use the encrypted settings collection for saved channel preferences")]
#[utoipa::path(responses((status = OK, body = Vec<ChannelPlaybackSpeed>)), security(("api_jwt_token" = [])))]
#[get("/")]
async fn get_channel_playback_speeds(
    account: Account,
    pool: WebData,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);
    let speeds = get_channel_playback_speeds_by_account_id(&mut conn, &account.id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;

    Ok(HttpResponse::Ok()
        .insert_header(DEPRECATION_HEADER)
        .json(speeds))
}

/// Save a channel playback speed through the deprecated dedicated API.
///
/// New OpenTubeX clients sync saved channel preferences through the encrypted
/// `settings` collection. This endpoint remains available for older clients.
#[deprecated(note = "use the encrypted settings collection for saved channel preferences")]
#[utoipa::path(responses((status = OK, body = ChannelPlaybackSpeed)), security(("api_jwt_token" = [])))]
#[put("/")]
async fn put_channel_playback_speed(
    account: Account,
    pool: WebData,
    speed: web::Json<ChannelPlaybackSpeed>,
) -> HandlerResult<impl Responder> {
    let mut speed = speed.into_inner();
    if speed.channel_id.is_empty()
        || !speed.playback_speed.is_finite()
        || speed.playback_speed <= 0.07
    {
        return Err(HandlerError::ValidationError);
    }

    speed.account_id = account.id;
    let mut conn = get_db_conn!(pool);
    store_playback_speed(&mut conn, &speed).await?;

    Ok(HttpResponse::Ok()
        .insert_header(DEPRECATION_HEADER)
        .json(speed))
}

/// Store a playback speed, enforcing the row quota in the same transaction.
///
/// Checking and writing separately would let two concurrent requests both pass
/// the check and then both insert, pushing the account past the quota.
async fn store_playback_speed(
    conn: &mut DbConnection,
    speed: &ChannelPlaybackSpeed,
) -> HandlerResult<()> {
    conn.transaction::<_, HandlerError, _>(|conn| {
        async move {
            set_channel_playback_speed(conn, speed)
                .await
                .map_err(|_| HandlerError::InternalDatabaseError)?;

            // Authoritative check on the rows that now exist. This is an upsert,
            // so overwriting an existing speed must not be charged as a new row;
            // an error here rolls the write back.
            check_stored_rows(
                count_playback_speeds(conn, &speed.account_id)
                    .await
                    .map_err(|_| HandlerError::InternalDatabaseError)?,
            )?;
            Ok(())
        }
        .scope_boxed()
    })
    .await
}

/// Delete a channel playback speed through the deprecated dedicated API.
///
/// New OpenTubeX clients sync saved channel preferences through the encrypted
/// `settings` collection. This endpoint remains available for older clients.
#[deprecated(note = "use the encrypted settings collection for saved channel preferences")]
#[utoipa::path(responses((status = OK)), security(("api_jwt_token" = [])))]
#[delete("/{channel_id}")]
async fn delete_channel_playback_speed(
    account: Account,
    pool: WebData,
    channel_id: web::Path<String>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);
    remove_channel_playback_speed(&mut conn, &account.id, &channel_id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;

    Ok(HttpResponse::Ok()
        .insert_header(DEPRECATION_HEADER)
        .finish())
}
