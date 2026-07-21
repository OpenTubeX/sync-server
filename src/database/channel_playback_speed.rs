use diesel::{BoolExpressionMethods, ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;

use crate::{
    DbConnection, database::DbError, models::ChannelPlaybackSpeed,
    schema::channel_playback_speed::dsl::*,
};

pub async fn get_channel_playback_speeds_by_account_id(
    conn: &mut DbConnection,
    account_id_: &str,
) -> Result<Vec<ChannelPlaybackSpeed>, DbError> {
    channel_playback_speed
        .filter(account_id.eq(account_id_))
        .select(ChannelPlaybackSpeed::as_select())
        .load(conn)
        .await
}

pub async fn set_channel_playback_speed(
    conn: &mut DbConnection,
    speed: &ChannelPlaybackSpeed,
) -> Result<(), DbError> {
    diesel::insert_into(channel_playback_speed)
        .values(speed)
        .on_conflict((account_id, channel_id))
        .do_update()
        .set(speed)
        .execute(conn)
        .await?;

    Ok(())
}

pub async fn remove_channel_playback_speed(
    conn: &mut DbConnection,
    account_id_: &str,
    channel_id_: &str,
) -> Result<(), DbError> {
    diesel::delete(
        channel_playback_speed.filter(account_id.eq(account_id_).and(channel_id.eq(channel_id_))),
    )
    .execute(conn)
    .await?;

    Ok(())
}
