#![allow(deprecated)]

use actix_web::{
    HttpResponse, Responder,
    body::MessageBody,
    delete,
    dev::{ServiceRequest, ServiceResponse},
    get,
    http::header::{HeaderName, HeaderValue},
    middleware::{Next, from_fn},
    put, web,
};
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

const DEPRECATION_DATE: &str = "@1786665600";

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
            .wrap(from_fn(deprecation_middleware))
            .service(get_channel_playback_speeds)
            .service(put_channel_playback_speed)
            .service(delete_channel_playback_speed)
    }
}

async fn deprecation_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let mut response = next.call(req).await?;
    response.headers_mut().insert(
        HeaderName::from_static("deprecation"),
        HeaderValue::from_static(DEPRECATION_DATE),
    );
    Ok(response)
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

    Ok(HttpResponse::Ok().json(speeds))
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

    Ok(HttpResponse::Ok().json(speed))
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

    Ok(HttpResponse::Ok().finish())
}

#[cfg(test)]
mod tests {
    use actix_web::{App, HttpResponse, http::StatusCode, middleware::from_fn, test, web};

    use super::{DEPRECATION_DATE, deprecation_middleware};
    use crate::handlers::{HandlerError, HandlerResult};

    async fn success() -> HttpResponse {
        HttpResponse::Ok().finish()
    }

    async fn failure() -> HandlerResult<HttpResponse> {
        Err(HandlerError::ValidationError)
    }

    #[actix_web::test]
    async fn deprecation_header_is_added_to_success_and_error_responses() {
        let app = test::init_service(
            App::new()
                .wrap(from_fn(deprecation_middleware))
                .route("/success", web::get().to(success))
                .route("/failure", web::get().to(failure)),
        )
        .await;

        for (path, expected_status) in [
            ("/success", StatusCode::OK),
            ("/failure", StatusCode::BAD_REQUEST),
        ] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;

            assert_eq!(response.status(), expected_status);
            assert_eq!(
                response
                    .headers()
                    .get("deprecation")
                    .expect("deprecation header should be present"),
                DEPRECATION_DATE
            );
        }
    }
}
