use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};

use crate::DbConnection;
use crate::database::DbError;
use crate::models::EncryptedSync;
use crate::schema::encrypted_sync::dsl::{
    account_id, collection, encrypted_sync, payload as encrypted_payload,
    revision as encrypted_revision,
};

#[derive(Debug, PartialEq, Eq)]
pub enum SaveResult {
    Saved,
    Conflict,
    QuotaExceeded,
}

#[derive(QueryableByName)]
pub struct LegacyEncryptedSync {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub revision: i64,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub payload: String,
}

#[derive(QueryableByName)]
struct StoredCollection {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    revision: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    bytes: i64,
}

#[derive(QueryableByName)]
struct StoredBytes {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    bytes: i64,
}

#[cfg(feature = "sqlite")]
async fn get_stored_collection(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
) -> Result<Option<StoredCollection>, DbError> {
    diesel::sql_query(
        "SELECT revision, CAST(LENGTH(CAST(payload AS BLOB)) AS BIGINT) AS bytes \
         FROM encrypted_sync WHERE account_id = ? AND collection = ?",
    )
    .bind::<diesel::sql_types::Text, _>(owner_id)
    .bind::<diesel::sql_types::Text, _>(collection_name)
    .get_result(conn)
    .await
    .optional()
}

#[cfg(feature = "postgres")]
async fn get_stored_collection(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
) -> Result<Option<StoredCollection>, DbError> {
    diesel::sql_query(
        "SELECT revision, OCTET_LENGTH(payload)::BIGINT AS bytes \
         FROM encrypted_sync WHERE account_id = $1 AND collection = $2",
    )
    .bind::<diesel::sql_types::Text, _>(owner_id)
    .bind::<diesel::sql_types::Text, _>(collection_name)
    .get_result(conn)
    .await
    .optional()
}

#[cfg(feature = "sqlite")]
async fn get_stored_bytes(conn: &mut DbConnection, owner_id: &str) -> Result<i64, DbError> {
    diesel::sql_query(
        "SELECT CAST(COALESCE(SUM(LENGTH(CAST(payload AS BLOB))), 0) AS BIGINT) AS bytes \
         FROM encrypted_sync WHERE account_id = ?",
    )
    .bind::<diesel::sql_types::Text, _>(owner_id)
    .get_result::<StoredBytes>(conn)
    .await
    .map(|result| result.bytes)
}

fn exceeds_storage_quota(
    stored_bytes: i64,
    previous_bytes: i64,
    new_bytes: usize,
    max_bytes: usize,
) -> bool {
    let Ok(stored_bytes) = usize::try_from(stored_bytes) else {
        return true;
    };
    let Ok(previous_bytes) = usize::try_from(previous_bytes) else {
        return true;
    };

    stored_bytes
        .saturating_sub(previous_bytes)
        .saturating_add(new_bytes)
        > max_bytes
}

#[cfg(feature = "postgres")]
async fn get_stored_bytes(conn: &mut DbConnection, owner_id: &str) -> Result<i64, DbError> {
    diesel::sql_query(
        "SELECT COALESCE(SUM(OCTET_LENGTH(payload)), 0)::BIGINT AS bytes \
         FROM encrypted_sync WHERE account_id = $1",
    )
    .bind::<diesel::sql_types::Text, _>(owner_id)
    .get_result::<StoredBytes>(conn)
    .await
    .map(|result| result.bytes)
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

pub async fn save(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
    expected_revision: i64,
    new_payload: &str,
    max_account_bytes: usize,
) -> Result<SaveResult, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            use crate::schema::account;

            // Serialize quota checks for this account across processes and database backends.
            diesel::update(account::table.filter(account::id.eq(owner_id)))
                .set(account::id.eq(account::id))
                .execute(conn)
                .await?;

            let existing = get_stored_collection(conn, owner_id, collection_name).await?;
            if existing
                .as_ref()
                .map_or(expected_revision != 0, |document| {
                    document.revision != expected_revision
                })
            {
                return Ok(SaveResult::Conflict);
            }

            let stored_bytes = get_stored_bytes(conn, owner_id).await?;
            let previous_bytes = existing.as_ref().map_or(0, |document| document.bytes);
            if exceeds_storage_quota(
                stored_bytes,
                previous_bytes,
                new_payload.len(),
                max_account_bytes,
            ) {
                return Ok(SaveResult::QuotaExceeded);
            }

            if existing.is_some() {
                diesel::update(
                    encrypted_sync
                        .filter(account_id.eq(owner_id))
                        .filter(collection.eq(collection_name)),
                )
                .set((
                    encrypted_revision.eq(expected_revision + 1),
                    encrypted_payload.eq(new_payload),
                ))
                .execute(conn)
                .await?;
            } else {
                create(
                    conn,
                    &EncryptedSync {
                        account_id: owner_id.to_owned(),
                        collection: collection_name.to_owned(),
                        revision: 1,
                        payload: new_payload.to_owned(),
                    },
                )
                .await?;
                clear_legacy_collection(conn, owner_id, collection_name).await?;
            }

            Ok(SaveResult::Saved)
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

#[cfg(test)]
mod tests {
    use super::exceeds_storage_quota;

    #[test]
    fn storage_quota_accounts_for_replaced_payload() {
        assert!(!exceeds_storage_quota(120, 20, 28, 128));
        assert!(exceeds_storage_quota(120, 20, 29, 128));
    }

    #[test]
    fn storage_quota_rejects_invalid_database_sizes() {
        assert!(exceeds_storage_quota(-1, 0, 1, 128));
        assert!(exceeds_storage_quota(1, -1, 1, 128));
    }
}
