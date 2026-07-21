use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};

use crate::DbConnection;
use crate::database::DbError;
use crate::models::EncryptedSync;
use crate::schema::encrypted_sync::dsl::*;

pub async fn get(
    conn: &mut DbConnection,
    owner_id: &str,
) -> Result<Option<EncryptedSync>, DbError> {
    encrypted_sync
        .filter(account_id.eq(owner_id))
        .select(EncryptedSync::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn create(conn: &mut DbConnection, document: &EncryptedSync) -> Result<(), DbError> {
    diesel::insert_into(encrypted_sync)
        .values(document)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn create_and_clear_legacy(
    conn: &mut DbConnection,
    document: &EncryptedSync,
) -> Result<(), DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            clear_legacy_data(conn, &document.account_id).await?;
            create(conn, document).await
        })
    })
    .await
}

/// Remove account-linked plaintext data when an account first switches to
/// encrypted sync. Shared channel/video metadata may remain for other users,
/// but is no longer associated with this account.
pub async fn clear_legacy_data(conn: &mut DbConnection, owner_id: &str) -> Result<(), DbError> {
    use crate::schema::{
        channel_playback_speed, playlist, playlist_bookmark, playlist_video_member, subscription,
        subscription_group, watch_history,
    };

    diesel::delete(
        channel_playback_speed::table.filter(channel_playback_speed::account_id.eq(owner_id)),
    )
    .execute(conn)
    .await?;
    diesel::delete(playlist_bookmark::table.filter(playlist_bookmark::account_id.eq(owner_id)))
        .execute(conn)
        .await?;
    diesel::delete(
        playlist_video_member::table.filter(playlist_video_member::account_id.eq(owner_id)),
    )
    .execute(conn)
    .await?;
    diesel::delete(playlist::table.filter(playlist::account_id.eq(owner_id)))
        .execute(conn)
        .await?;
    diesel::delete(subscription_group::table.filter(subscription_group::account_id.eq(owner_id)))
        .execute(conn)
        .await?;
    diesel::delete(watch_history::table.filter(watch_history::account_id.eq(owner_id)))
        .execute(conn)
        .await?;
    diesel::delete(subscription::table.filter(subscription::account_id.eq(owner_id)))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn has_legacy_data(conn: &mut DbConnection, owner_id: &str) -> Result<bool, DbError> {
    use crate::schema::{
        channel_playback_speed, playlist, playlist_bookmark, playlist_video_member, subscription,
        subscription_group, watch_history,
    };
    use diesel::dsl::exists;

    if diesel::select(exists(
        subscription::table.filter(subscription::account_id.eq(owner_id)),
    ))
    .get_result(conn)
    .await?
    {
        return Ok(true);
    }
    if diesel::select(exists(
        playlist::table.filter(playlist::account_id.eq(owner_id)),
    ))
    .get_result(conn)
    .await?
    {
        return Ok(true);
    }
    if diesel::select(exists(
        playlist_video_member::table.filter(playlist_video_member::account_id.eq(owner_id)),
    ))
    .get_result(conn)
    .await?
    {
        return Ok(true);
    }
    if diesel::select(exists(
        playlist_bookmark::table.filter(playlist_bookmark::account_id.eq(owner_id)),
    ))
    .get_result(conn)
    .await?
    {
        return Ok(true);
    }
    if diesel::select(exists(
        watch_history::table.filter(watch_history::account_id.eq(owner_id)),
    ))
    .get_result(conn)
    .await?
    {
        return Ok(true);
    }
    if diesel::select(exists(
        subscription_group::table.filter(subscription_group::account_id.eq(owner_id)),
    ))
    .get_result(conn)
    .await?
    {
        return Ok(true);
    }
    diesel::select(exists(
        channel_playback_speed::table.filter(channel_playback_speed::account_id.eq(owner_id)),
    ))
    .get_result(conn)
    .await
}

pub async fn replace(
    conn: &mut DbConnection,
    owner_id: &str,
    expected_revision: i64,
    new_payload: &str,
) -> Result<bool, DbError> {
    let updated = diesel::update(
        encrypted_sync
            .filter(account_id.eq(owner_id))
            .filter(revision.eq(expected_revision)),
    )
    .set((revision.eq(expected_revision + 1), payload.eq(new_payload)))
    .execute(conn)
    .await?;
    Ok(updated == 1)
}
