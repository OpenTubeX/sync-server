use actix_web::{HttpResponse, Responder, delete, get, middleware::from_fn, patch, put, web};
use diesel_async::AsyncConnection;
use serde::Deserialize;
use utoipa_actix_web::scope;

use crate::{
    WebData,
    database::{
        channel::create_or_update_channel,
        quota::{count_watch_history, lock_account_quota},
        video::{create_or_update_video, create_video_if_missing},
        watch_history::{
            DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, add_or_update_video_to_watch_history,
            clear_watch_history_by_account_id, get_watch_history_by_account_id,
            get_watch_history_entry, remove_video_from_watch_history,
        },
    },
    dto::{CreateVideo, ExtendedWatchHistoryItem, WatchedState},
    get_db_conn,
    handlers::{
        HandlerError, HandlerResult, ScopedHandler, check_already_over_quota, check_bulk_size,
        check_stored_rows, user::auth_middleware,
    },
    models::{Account, Channel, WatchHistoryItem},
    validation::{validate_videos_against_youtube, videos_requiring_validation},
};

pub struct WatchHistoryHandler;
impl ScopedHandler for WatchHistoryHandler {
    fn get_service() -> utoipa_actix_web::scope::Scope<
        impl actix_web::dev::ServiceFactory<
            actix_web::dev::ServiceRequest,
            Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        >,
    > {
        scope::scope("/watch_history")
            .wrap(from_fn(auth_middleware))
            .service(get_watch_history)
            .service(add_to_watch_history_bulk)
            .service(get_from_watch_history)
            .service(add_to_watch_history)
            .service(update_watch_history_video_state)
            .service(remove_from_watch_history)
            .service(clear_watch_history)
    }
}

/// An item ready to persist, and whether its video metadata may replace the
/// stored copy (see [`validate_videos_against_youtube`]).
struct PreparedItem {
    item: ExtendedWatchHistoryItem,
    verified: bool,
}

/// Stamp ownership onto a batch of items and validate their videos.
///
/// Validation is grouped by uploader, so a batch costs one YouTube round-trip
/// per distinct channel rather than one per item, and no pooled connection is
/// held while those round-trips happen.
async fn prepare_watch_history_items(
    pool: &WebData,
    account_id: &str,
    mut items: Vec<ExtendedWatchHistoryItem>,
) -> HandlerResult<Vec<PreparedItem>> {
    for item in &mut items {
        item.metadata.account_id = account_id.to_string();
        item.metadata.video_id = item.video.id.clone();
    }

    // group item indices by uploader, preserving the caller's ordering
    let mut groups: Vec<(Channel, Vec<usize>)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match groups
            .iter_mut()
            .find(|(channel, _)| *channel == item.video.uploader)
        {
            Some((_, indices)) => indices.push(index),
            None => groups.push((item.video.uploader.clone(), vec![index])),
        }
    }

    let mut grouped_videos: Vec<Vec<CreateVideo>> = groups
        .iter()
        .map(|(_, indices)| indices.iter().map(|i| items[*i].video.clone()).collect())
        .collect();

    let mut needs_validation = Vec::with_capacity(groups.len());
    {
        let mut conn = get_db_conn!(pool);
        check_already_over_quota(
            count_watch_history(&mut conn, account_id)
                .await
                .map_err(|_| HandlerError::InternalDatabaseError)?,
        )?;
        for videos in &grouped_videos {
            needs_validation.push(videos_requiring_validation(&mut conn, videos).await);
        }
    }

    let mut verified = vec![true; items.len()];
    for (((channel, indices), videos), needs_validation) in groups
        .iter_mut()
        .zip(grouped_videos.iter_mut())
        .zip(&needs_validation)
    {
        let group_verified =
            validate_videos_against_youtube(videos, needs_validation, channel).await?;
        for (index, group_verified) in indices.iter().zip(group_verified) {
            verified[*index] = group_verified;
        }
    }

    // write the validated metadata back onto the items
    for ((_, indices), videos) in groups.iter().zip(&grouped_videos) {
        for (index, video) in indices.iter().zip(videos) {
            items[*index].video = video.clone();
        }
    }

    Ok(items
        .into_iter()
        .zip(verified)
        .map(|(item, verified)| PreparedItem { item, verified })
        .collect())
}

async fn persist_watch_history_item(
    conn: &mut crate::DbConnection,
    PreparedItem { item, verified }: &PreparedItem,
) -> HandlerResult<()> {
    create_or_update_channel(conn, &item.video.uploader)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;
    let video = (&item.video).into();
    if *verified {
        create_or_update_video(conn, &video).await
    } else {
        create_video_if_missing(conn, &video).await
    }
    .map_err(|_| HandlerError::InternalDatabaseError)?;
    add_or_update_video_to_watch_history(conn, &item.metadata)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?;

    Ok(())
}

async fn persist_watch_history_items(
    conn: &mut crate::DbConnection,
    account_id: &str,
    items: &[PreparedItem],
) -> HandlerResult<()> {
    conn.transaction::<_, HandlerError, _>(async |conn| {
        lock_account_quota(conn, account_id)
            .await
            .map_err(|_| HandlerError::InternalDatabaseError)?;
        for item in items {
            persist_watch_history_item(conn, item).await?;
        }

        // Authoritative check on the rows that now exist. History entries are
        // upserts, so counting afterwards is the only way to charge for what
        // was actually added; an error here rolls the batch back.
        check_stored_rows(
            count_watch_history(conn, account_id)
                .await
                .map_err(|_| HandlerError::InternalDatabaseError)?,
        )?;
        Ok(())
    })
    .await
}

async fn store_watch_history_items(
    pool: &WebData,
    account_id: &str,
    items: Vec<ExtendedWatchHistoryItem>,
) -> HandlerResult<()> {
    let items = prepare_watch_history_items(pool, account_id, items).await?;

    let mut conn = get_db_conn!(pool);
    persist_watch_history_items(&mut conn, account_id, &items).await
}

#[utoipa::path(responses((status = OK)), security(("api_jwt_token" = [])))]
#[put("/bulk")]
async fn add_to_watch_history_bulk(
    account: Account,
    pool: WebData,
    items: web::Json<Vec<ExtendedWatchHistoryItem>>,
) -> HandlerResult<impl Responder> {
    let items = items.into_inner();
    check_bulk_size(items.len())?;

    store_watch_history_items(&pool, &account.id, items).await?;

    Ok(HttpResponse::Ok())
}

#[derive(Deserialize, Eq, PartialEq, PartialOrd, Ord, utoipa::ToSchema)]
enum WatchHistoryOrder {
    #[serde(rename = "added_date_asc")]
    AddedDateAscending,
    #[serde(rename = "added_date_desc")]
    AddedDateDescending,
}
#[derive(Deserialize, utoipa::IntoParams)]
struct WatchHistoryPaginationRequest {
    page: Option<u32>,
    page_size: Option<u32>,
    state: Option<WatchedState>,
    order: Option<WatchHistoryOrder>,
}

#[utoipa::path(responses((status = OK, body = Vec<ExtendedWatchHistoryItem>)), params(WatchHistoryPaginationRequest), security(("api_jwt_token" = [])))]
#[get("/")]
async fn get_watch_history(
    account: Account,
    pool: WebData,
    params: web::Query<WatchHistoryPaginationRequest>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    let watched_state = params
        .state
        .clone()
        .map(|s| serde_json::ser::to_string(&s).unwrap());
    let page_size = params
        .page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);

    match get_watch_history_by_account_id(
        &mut conn,
        &account.id,
        params.page.unwrap_or(1),
        page_size,
        &watched_state,
        params.order == Some(WatchHistoryOrder::AddedDateAscending),
    )
    .await
    {
        Ok(history) => {
            let history = history
                .iter()
                .map(|(metadata, video, channel)| ExtendedWatchHistoryItem {
                    video: CreateVideo::from((video, channel)),
                    metadata: metadata.clone(),
                })
                .collect::<Vec<_>>();
            Ok(HttpResponse::Ok().json(history))
        }
        Err(err) => Err(HandlerError::InternalDatabaseErrorWithContext(
            err.to_string(),
        )),
    }
}

#[utoipa::path(responses((status = OK, body = ExtendedWatchHistoryItem)), security(("api_jwt_token" = [])))]
#[get("/{video_id}")]
async fn get_from_watch_history(
    account: Account,
    pool: WebData,
    video_id: web::Path<String>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    match get_watch_history_entry(&mut conn, &account.id, &video_id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?
    {
        Some((metadata, video, channel)) => Ok(HttpResponse::Ok().json(ExtendedWatchHistoryItem {
            video: CreateVideo::from((&video, &channel)),
            metadata: metadata.clone(),
        })),
        None => Err(HandlerError::NotInWatchHistory),
    }
}

#[utoipa::path(responses((status = CREATED, body = ExtendedWatchHistoryItem)), security(("api_jwt_token" = [])))]
#[put("/")]
async fn add_to_watch_history(
    account: Account,
    pool: WebData,
    watch_history_item: web::Json<ExtendedWatchHistoryItem>,
) -> HandlerResult<impl Responder> {
    let video_id = watch_history_item.video.id.clone();
    store_watch_history_items(&pool, &account.id, vec![watch_history_item.into_inner()]).await?;

    // Answer with what was stored: the submitted metadata may have been
    // normalized, or kept out in favor of a stored copy.
    let mut conn = get_db_conn!(pool);
    let (metadata, video, channel) = get_watch_history_entry(&mut conn, &account.id, &video_id)
        .await
        .map_err(|_| HandlerError::InternalDatabaseError)?
        .ok_or(HandlerError::InternalDatabaseError)?;

    Ok(HttpResponse::Ok().json(ExtendedWatchHistoryItem {
        video: CreateVideo::from((&video, &channel)),
        metadata,
    }))
}

#[utoipa::path(responses((status = OK)), security(("api_jwt_token" = [])))]
#[patch("/{video_id}")]
async fn update_watch_history_video_state(
    account: Account,
    pool: WebData,
    watch_history_item: web::Json<WatchHistoryItem>,
    video_id: web::Path<String>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    let mut watch_history_item = watch_history_item.into_inner();
    watch_history_item.video_id = video_id.into_inner();
    watch_history_item.account_id = account.id;

    // This adds the entry if it doesn't exist yet, so it's subject to the quota.
    conn.transaction::<_, HandlerError, _>(async |conn| {
        let (item, account_id) = (&watch_history_item, &watch_history_item.account_id);
        lock_account_quota(conn, account_id)
            .await
            .map_err(|_| HandlerError::InternalDatabaseError)?;
        add_or_update_video_to_watch_history(conn, item)
            .await
            .map_err(|_| HandlerError::InternalDatabaseError)?;
        check_stored_rows(
            count_watch_history(conn, account_id)
                .await
                .map_err(|_| HandlerError::InternalDatabaseError)?,
        )?;
        Ok(())
    })
    .await?;

    Ok(HttpResponse::Ok().json(watch_history_item))
}

#[utoipa::path(responses((status = OK)), security(("api_jwt_token" = [])))]
#[delete("/")]
async fn clear_watch_history(account: Account, pool: WebData) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    match clear_watch_history_by_account_id(&mut conn, &account.id).await {
        Ok(()) => Ok(HttpResponse::Ok()),
        Err(err) => Err(HandlerError::InternalDatabaseErrorWithContext(
            err.to_string(),
        )),
    }
}

#[utoipa::path(responses((status = OK)), security(("api_jwt_token" = [])))]
#[delete("/{video_id}")]
async fn remove_from_watch_history(
    account: Account,
    pool: WebData,
    video_id: web::Path<String>,
) -> HandlerResult<impl Responder> {
    let mut conn = get_db_conn!(pool);

    match remove_video_from_watch_history(&mut conn, &account.id, &video_id).await {
        Ok(()) => Ok(HttpResponse::Ok()),
        Err(err) => Err(HandlerError::InternalDatabaseErrorWithContext(
            err.to_string(),
        )),
    }
}
