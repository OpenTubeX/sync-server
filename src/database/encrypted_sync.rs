use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};

use crate::DbConnection;
use crate::database::DbError;
use crate::models::EncryptedSync;
use crate::schema::encrypted_sync::dsl::{
    account_id, collection, encrypted_sync, payload as encrypted_payload,
    revision as encrypted_revision,
};

#[derive(QueryableByName)]
pub struct LegacyEncryptedSync {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub revision: i64,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub payload: String,
}

pub async fn get_all(
    conn: &mut DbConnection,
    owner_id: &str,
) -> Result<Vec<EncryptedSync>, DbError> {
    encrypted_sync
        .filter(account_id.eq(owner_id))
        .select(EncryptedSync::as_select())
        .load(conn)
        .await
}

pub async fn get(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
) -> Result<Option<EncryptedSync>, DbError> {
    encrypted_sync
        .filter(account_id.eq(owner_id).and(collection.eq(collection_name)))
        .select(EncryptedSync::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn exists(conn: &mut DbConnection, owner_id: &str) -> Result<bool, DbError> {
    use diesel::dsl::exists as query_exists;

    if diesel::select(query_exists(encrypted_sync.filter(account_id.eq(owner_id))))
        .get_result(conn)
        .await?
    {
        return Ok(true);
    }

    Ok(get_legacy_encrypted(conn, owner_id).await?.is_some())
}

pub async fn get_legacy_encrypted(
    conn: &mut DbConnection,
    owner_id: &str,
) -> Result<Option<LegacyEncryptedSync>, DbError> {
    diesel::sql_query(
        "SELECT revision, payload FROM encrypted_sync_single_document WHERE account_id = ?",
    )
    .bind::<diesel::sql_types::Text, _>(owner_id)
    .get_result(conn)
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

pub async fn create_initial(
    conn: &mut DbConnection,
    document: &EncryptedSync,
) -> Result<(), DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            create(conn, document).await?;
            clear_legacy_collection(conn, &document.account_id, &document.collection).await
        })
    })
    .await
}

/// Remove a collection's account-linked plaintext data only after the matching
/// ciphertext has been stored. Shared channel/video metadata may remain for
/// other users, but is no longer associated with this account.
pub async fn clear_legacy_collection(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
) -> Result<(), DbError> {
    use crate::schema::{
        channel_playback_speed, playlist, playlist_bookmark, playlist_video_member, subscription,
        subscription_group, watch_history,
    };

    match collection_name {
        "subscriptions" => {
            diesel::delete(subscription::table.filter(subscription::account_id.eq(owner_id)))
                .execute(conn)
                .await?;
        }
        "playlists" => {
            diesel::delete(
                playlist_video_member::table.filter(playlist_video_member::account_id.eq(owner_id)),
            )
            .execute(conn)
            .await?;
            diesel::delete(playlist::table.filter(playlist::account_id.eq(owner_id)))
                .execute(conn)
                .await?;
        }
        "history" => {
            diesel::delete(watch_history::table.filter(watch_history::account_id.eq(owner_id)))
                .execute(conn)
                .await?;
        }
        "playbackSpeeds" => {
            diesel::delete(
                channel_playback_speed::table
                    .filter(channel_playback_speed::account_id.eq(owner_id)),
            )
            .execute(conn)
            .await?;
        }
        "profiles" => {
            diesel::delete(
                subscription_group::table.filter(subscription_group::account_id.eq(owner_id)),
            )
            .execute(conn)
            .await?;
        }
        "playlistBookmarks" => {
            diesel::delete(
                playlist_bookmark::table.filter(playlist_bookmark::account_id.eq(owner_id)),
            )
            .execute(conn)
            .await?;
        }
        _ => {}
    }
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
    collection_name: &str,
    expected_revision: i64,
    new_payload: &str,
) -> Result<bool, DbError> {
    let updated = diesel::update(
        encrypted_sync
            .filter(account_id.eq(owner_id))
            .filter(collection.eq(collection_name))
            .filter(encrypted_revision.eq(expected_revision)),
    )
    .set((
        encrypted_revision.eq(expected_revision + 1),
        encrypted_payload.eq(new_payload),
    ))
    .execute(conn)
    .await?;
    Ok(updated == 1)
}
