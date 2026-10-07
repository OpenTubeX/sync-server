use diesel::{
    BoolExpressionMethods, ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper,
};
use diesel_async::RunQueryDsl as _;

use crate::{
    DbConnection,
    database::DbError,
    models::{Channel, Video, WatchHistoryItem},
    schema::{channel, video, watch_history::dsl::*},
};

pub const DEFAULT_PAGE_SIZE: u32 = 50;
pub const MAX_PAGE_SIZE: u32 = 1000;

pub async fn get_watch_history_by_account_id(
    conn: &mut DbConnection,
    account_id_: &str,
    page_num: u32,
    page_size: u32,
    state: &Option<String>,
    sort_by_date_ascending: bool,
) -> Result<Vec<(WatchHistoryItem, Video, Channel)>, DbError> {
    // https://github.com/diesel-rs/diesel/issues/455
    let mut query = watch_history
        .filter(account_id.eq(account_id_))
        .into_boxed();

    if let Some(state) = &state {
        query = query.filter(watched_state.eq(state));
    }

    if sort_by_date_ascending {
        query = query.order(added_date.asc())
    } else {
        query = query.order(added_date.desc())
    }

    let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
    query
        .offset(i64::from(page_size) * i64::from(page_num.saturating_sub(1)))
        .limit(i64::from(page_size))
        .inner_join(video::table.inner_join(channel::table))
        .select((
            WatchHistoryItem::as_select(),
            Video::as_select(),
            Channel::as_select(),
        ))
        .load(conn)
        .await
}

pub async fn get_watch_history_entry(
    conn: &mut DbConnection,
    account_id_: &str,
    video_id_: &str,
) -> Result<Option<(WatchHistoryItem, Video, Channel)>, DbError> {
    watch_history
        .filter(account_id.eq(account_id_).and(video_id.eq(video_id_)))
        .inner_join(video::table.inner_join(channel::table))
        .select((
            WatchHistoryItem::as_select(),
            Video::as_select(),
            Channel::as_select(),
        ))
        .first(conn)
        .await
        .optional()
}

pub async fn add_or_update_video_to_watch_history(
    conn: &mut DbConnection,
    watch_history_item_: &WatchHistoryItem,
) -> Result<(), DbError> {
    diesel::insert_into(watch_history)
        .values(watch_history_item_)
        .on_conflict((video_id, account_id))
        .do_update()
        .set(watch_history_item_)
        .execute(conn)
        .await?;

    Ok(())
}

pub async fn remove_video_from_watch_history(
    conn: &mut DbConnection,
    account_id_: &str,
    video_id_: &str,
) -> Result<(), DbError> {
    diesel::delete(
        watch_history.filter(
            account_id
                .eq(account_id_.to_string())
                .and(video_id.eq(video_id_.to_string())),
        ),
    )
    .execute(conn)
    .await?;

    Ok(())
}

pub async fn clear_watch_history_by_account_id(
    conn: &mut DbConnection,
    account_id_: &str,
) -> Result<(), DbError> {
    diesel::delete(watch_history.filter(account_id.eq(account_id_.to_string())))
        .execute(conn)
        .await?;

    Ok(())
}
