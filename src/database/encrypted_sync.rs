use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};

use crate::DbConnection;
use crate::database::DbError;
use crate::models::EncryptedSync;
use crate::schema::encrypted_sync::dsl::{
    account_id, base_revision, collection, encrypted_sync, payload as encrypted_payload,
    revision as encrypted_revision,
};
use crate::schema::encrypted_sync_change;

/// Appends allowed before a client must upload the full collection again.
pub const MAX_COLLECTION_CHANGES: i64 = 64;
/// A change log may grow to the size of its collection, but small collections
/// may still collect this many bytes before a full upload is required.
const MIN_CHANGE_LOG_BYTES: i64 = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum SaveResult {
    Saved,
    Conflict,
    QuotaExceeded,
    /// Changes cannot be appended; the client must upload the full collection.
    FullUploadRequired,
}

/// Collections stored before change logs existed keep `base_revision` 0,
/// because filling it in would rewrite each row. They have no changes yet.
fn base_or_revision(base: i64, revision: i64) -> i64 {
    if base == 0 { revision } else { base }
}

/// A collection read for a client that may already hold an older revision.
#[derive(Debug, PartialEq, Eq)]
pub struct CollectionRead {
    pub revision: i64,
    pub base_revision: i64,
    /// Omitted when the caller's revision is covered by `changes`.
    pub payload: Option<String>,
    /// Changes after the returned payload or the caller's revision, in order.
    pub changes: Vec<(i64, String)>,
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
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    change_bytes: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    changes: i64,
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
        "SELECT revision, CAST(OCTET_LENGTH(payload) AS BIGINT) AS bytes, \
         CAST((SELECT COALESCE(SUM(OCTET_LENGTH(change.payload)), 0) FROM encrypted_sync_change change \
           WHERE change.account_id = encrypted_sync.account_id AND change.collection = encrypted_sync.collection) AS BIGINT) AS change_bytes, \
         CAST((SELECT COUNT(*) FROM encrypted_sync_change change \
           WHERE change.account_id = encrypted_sync.account_id AND change.collection = encrypted_sync.collection) AS BIGINT) AS changes \
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
        "SELECT revision, OCTET_LENGTH(payload)::BIGINT AS bytes, \
         (SELECT COALESCE(SUM(OCTET_LENGTH(change.payload)), 0) FROM encrypted_sync_change change \
           WHERE change.account_id = encrypted_sync.account_id AND change.collection = encrypted_sync.collection)::BIGINT AS change_bytes, \
         (SELECT COUNT(*) FROM encrypted_sync_change change \
           WHERE change.account_id = encrypted_sync.account_id AND change.collection = encrypted_sync.collection)::BIGINT AS changes \
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
        "SELECT CAST(COALESCE((SELECT SUM(OCTET_LENGTH(payload)) FROM encrypted_sync WHERE account_id = ?), 0) \
         + COALESCE((SELECT SUM(OCTET_LENGTH(payload)) FROM encrypted_sync_change WHERE account_id = ?), 0) AS BIGINT) AS bytes",
    )
    .bind::<diesel::sql_types::Text, _>(owner_id)
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
        "SELECT (COALESCE((SELECT SUM(OCTET_LENGTH(payload)) FROM encrypted_sync WHERE account_id = $1), 0) \
         + COALESCE((SELECT SUM(OCTET_LENGTH(payload)) FROM encrypted_sync_change WHERE account_id = $1), 0))::BIGINT AS bytes",
    )
    .bind::<diesel::sql_types::Text, _>(owner_id)
    .get_result::<StoredBytes>(conn)
    .await
    .map(|result| result.bytes)
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
        "SELECT revision, payload FROM encrypted_sync_single_document WHERE account_id = $1",
    )
    .bind::<diesel::sql_types::Text, _>(owner_id)
    .get_result(conn)
    .await
    .optional()
}

pub async fn create(conn: &mut DbConnection, document: &EncryptedSync) -> Result<(), DbError> {
    diesel::insert_into(encrypted_sync)
        .values((document, base_revision.eq(document.revision)))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn revisions(
    conn: &mut DbConnection,
    owner: &str,
) -> Result<Vec<(String, i64)>, DbError> {
    encrypted_sync
        .filter(account_id.eq(owner))
        .order(collection.asc())
        .select((collection, encrypted_revision))
        .load(conn)
        .await
}

pub async fn save(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
    expected_revision: i64,
    new_payload: &str,
    max_account_bytes: usize,
    activity: Option<(&str, i64)>,
) -> Result<SaveResult, DbError> {
    conn.transaction(async |conn| {
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
        let previous_bytes = existing.as_ref().map_or(0, |document| {
            document.bytes.saturating_add(document.change_bytes)
        });
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
                base_revision.eq(expected_revision + 1),
                encrypted_payload.eq(new_payload),
            ))
            .execute(conn)
            .await?;
            // The full payload includes every appended change.
            diesel::delete(
                encrypted_sync_change::table
                    .filter(encrypted_sync_change::account_id.eq(owner_id))
                    .filter(encrypted_sync_change::collection.eq(collection_name)),
            )
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

        if let Some((payload, now)) = activity {
            crate::database::sync_event::append(conn, owner_id, "", payload, now).await?;
        }
        Ok(SaveResult::Saved)
    })
    .await
}

/// Read a collection for a client holding revision `since`. When the change log
/// covers that revision, only the later changes are returned. Otherwise the
/// full payload is returned with every change appended after it.
pub async fn read_since(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
    since: i64,
) -> Result<Option<CollectionRead>, DbError> {
    // PostgreSQL reads each statement at its own snapshot, so a concurrent full
    // upload can clear the change log between them. Retry inconsistent reads.
    for _ in 0..3 {
        let read = conn
            .transaction(async |conn| {
                let Some((revision, base)) = encrypted_sync
                    .filter(account_id.eq(owner_id).and(collection.eq(collection_name)))
                    .select((encrypted_revision, base_revision))
                    .first::<(i64, i64)>(conn)
                    .await
                    .optional()?
                else {
                    return Ok(Some(None));
                };
                let base = base_or_revision(base, revision);
                let incremental = since >= base && since <= revision;
                let payload = if incremental {
                    None
                } else {
                    Some(
                        encrypted_sync
                            .filter(account_id.eq(owner_id).and(collection.eq(collection_name)))
                            .filter(encrypted_revision.eq(revision))
                            .select(encrypted_payload)
                            .first::<String>(conn)
                            .await
                            .optional()?,
                    )
                };
                let first = if incremental { since } else { base };
                let changes: Vec<(i64, String)> = encrypted_sync_change::table
                    .filter(encrypted_sync_change::account_id.eq(owner_id))
                    .filter(encrypted_sync_change::collection.eq(collection_name))
                    .filter(encrypted_sync_change::revision.gt(first))
                    .filter(encrypted_sync_change::revision.le(revision))
                    .order(encrypted_sync_change::revision.asc())
                    .select((
                        encrypted_sync_change::revision,
                        encrypted_sync_change::payload,
                    ))
                    .load(conn)
                    .await?;
                let contiguous = changes
                    .iter()
                    .map(|(change, _)| *change)
                    .eq((first + 1)..=revision);
                Ok::<_, DbError>(match (payload, contiguous) {
                    (Some(None), _) | (_, false) => None,
                    (payload, true) => Some(Some(CollectionRead {
                        revision,
                        base_revision: base,
                        payload: payload.flatten(),
                        changes,
                    })),
                })
            })
            .await?;
        if let Some(read) = read {
            return Ok(read);
        }
    }
    Err(diesel::result::Error::RollbackTransaction)
}

/// The revision, base revision and full payload, read together so a concurrent
/// append cannot pair an older payload with a newer revision.
pub async fn get_full(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
) -> Result<Option<(i64, i64, String)>, DbError> {
    Ok(encrypted_sync
        .filter(account_id.eq(owner_id).and(collection.eq(collection_name)))
        .select((encrypted_revision, base_revision, encrypted_payload))
        .first::<(i64, i64, String)>(conn)
        .await
        .optional()?
        .map(|(revision, base, payload)| (revision, base_or_revision(base, revision), payload)))
}

/// Record that a session reads appended changes. Appends stay disabled while
/// any active session of the account has not.
pub async fn mark_session_reads_changes(
    conn: &mut DbConnection,
    session_id: &str,
) -> Result<(), DbError> {
    use crate::schema::account_session::dsl::{account_session, id, reads_collection_changes};

    let marked: Option<bool> = account_session
        .find(session_id)
        .select(reads_collection_changes)
        .first(conn)
        .await
        .optional()?;
    // Avoid a write, and with SQLite a write lock, on every read.
    if marked == Some(false) {
        diesel::update(account_session.filter(id.eq(session_id)))
            .set(reads_collection_changes.eq(true))
            .execute(conn)
            .await?;
    }
    Ok(())
}

/// Call while holding the account lock, so a concurrent password change cannot
/// replace the sessions between this check and the append.
async fn all_sessions_read_changes(
    conn: &mut DbConnection,
    owner_id: &str,
    now: i64,
) -> Result<bool, DbError> {
    use crate::schema::account;
    use crate::schema::account_session::dsl::{
        account_id as session_account_id, account_session, expires_at, generation,
        reads_collection_changes, revoked_at,
    };
    use diesel::dsl::exists;

    // An unused legacy token creates its session on first use, like a new
    // login. Until then, its reads without `since` fail instead of going stale.
    let session_generation: i64 = account::table
        .find(owner_id)
        .select(account::session_generation)
        .first(conn)
        .await?;
    // Provisional pairing sessions count too, as the paired device may be older.
    let blocked: bool = diesel::select(exists(
        account_session
            .filter(session_account_id.eq(owner_id))
            .filter(generation.eq(session_generation))
            .filter(expires_at.gt(now))
            .filter(revoked_at.is_null())
            .filter(reads_collection_changes.eq(false)),
    ))
    .get_result(conn)
    .await?;
    Ok(!blocked)
}

/// Append one encrypted change to a collection at `expected_revision`.
#[allow(clippy::too_many_arguments)]
pub async fn append_change(
    conn: &mut DbConnection,
    owner_id: &str,
    collection_name: &str,
    expected_revision: i64,
    new_payload: &str,
    max_account_bytes: usize,
    activity: Option<(&str, i64)>,
    now: i64,
) -> Result<SaveResult, DbError> {
    // Most accounts still have a session that cannot read changes. Answer those
    // without the account lock, which takes the SQLite write lock. Stale
    // revisions still get their conflict below, and the checks repeat under
    // the lock.
    let revision: Option<i64> = encrypted_sync
        .filter(account_id.eq(owner_id).and(collection.eq(collection_name)))
        .select(encrypted_revision)
        .first(conn)
        .await
        .optional()?;
    if revision == Some(expected_revision)
        && !all_sessions_read_changes(conn, owner_id, now).await?
    {
        return Ok(SaveResult::FullUploadRequired);
    }
    conn.transaction(async |conn| {
        use crate::schema::account;

        // Serialize with full uploads and quota checks for this account.
        diesel::update(account::table.filter(account::id.eq(owner_id)))
            .set(account::id.eq(account::id))
            .execute(conn)
            .await?;

        let Some(existing) = get_stored_collection(conn, owner_id, collection_name).await? else {
            return Ok(SaveResult::FullUploadRequired);
        };
        if existing.revision != expected_revision {
            return Ok(SaveResult::Conflict);
        }
        let Ok(new_bytes) = i64::try_from(new_payload.len()) else {
            return Ok(SaveResult::FullUploadRequired);
        };
        if existing.changes >= MAX_COLLECTION_CHANGES
            || existing.change_bytes.saturating_add(new_bytes)
                > existing.bytes.max(MIN_CHANGE_LOG_BYTES)
            || !all_sessions_read_changes(conn, owner_id, now).await?
        {
            return Ok(SaveResult::FullUploadRequired);
        }
        let stored_bytes = get_stored_bytes(conn, owner_id).await?;
        if exceeds_storage_quota(stored_bytes, 0, new_payload.len(), max_account_bytes) {
            return Ok(SaveResult::QuotaExceeded);
        }

        diesel::insert_into(encrypted_sync_change::table)
            .values((
                encrypted_sync_change::account_id.eq(owner_id),
                encrypted_sync_change::collection.eq(collection_name),
                encrypted_sync_change::revision.eq(expected_revision + 1),
                encrypted_sync_change::payload.eq(new_payload),
            ))
            .execute(conn)
            .await?;
        let filter = encrypted_sync
            .filter(account_id.eq(owner_id))
            .filter(collection.eq(collection_name));
        let base: i64 = filter.select(base_revision).first(conn).await?;
        diesel::update(filter)
            .set((
                encrypted_revision.eq(expected_revision + 1),
                base_revision.eq(base_or_revision(base, expected_revision)),
            ))
            .execute(conn)
            .await?;

        if let Some((payload, now)) = activity {
            crate::database::sync_event::append(conn, owner_id, "", payload, now).await?;
        }
        Ok(SaveResult::Saved)
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
        playlist, playlist_bookmark, playlist_video_member, subscription, subscription_group,
        watch_history,
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
        playlist, playlist_bookmark, playlist_video_member, subscription, subscription_group,
        watch_history,
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
    diesel::select(exists(
        subscription_group::table.filter(subscription_group::account_id.eq(owner_id)),
    ))
    .get_result(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::exceeds_storage_quota;

    #[actix_rt::test]
    #[cfg_attr(
        feature = "postgres",
        ignore = "requires TEST_DATABASE_URL pointing at a migrated PostgreSQL test database"
    )]
    async fn legacy_encrypted_lookup_returns_missing_and_stored_documents() {
        use diesel_async::{AsyncConnection, RunQueryDsl};

        use crate::{DbConnection, database::account, models::Account};

        #[cfg(feature = "sqlite")]
        let mut conn = {
            use diesel_migrations::MigrationHarness;

            let mut conn = DbConnection::establish(":memory:").await.unwrap();
            conn.spawn_blocking(|conn| {
                conn.run_pending_migrations(crate::MIGRATIONS).unwrap();
                Ok(())
            })
            .await
            .unwrap();
            conn
        };
        #[cfg(feature = "postgres")]
        let mut conn = DbConnection::establish(
            &std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be configured"),
        )
        .await
        .unwrap();

        let owner_id = uuid::Uuid::now_v7().to_string();
        let result: Result<(), diesel::result::Error> = conn
            .transaction(async |conn| {
                assert!(
                    super::get_legacy_encrypted(conn, &owner_id)
                        .await?
                        .is_none()
                );
                account::insert_new_account(
                    conn,
                    &Account {
                        id: owner_id.clone(),
                        name_hash: owner_id.clone(),
                        password_hash: None,
                        oidc_sub: None,
                        legacy_tokens_enabled: false,
                        session_generation: 0,
                    },
                )
                .await?;
                diesel::sql_query(
                    "INSERT INTO encrypted_sync_single_document (account_id, revision, payload) \
                     VALUES ($1, 7, 'legacy-payload')",
                )
                .bind::<diesel::sql_types::Text, _>(&owner_id)
                .execute(conn)
                .await?;
                let document = super::get_legacy_encrypted(conn, &owner_id)
                    .await?
                    .expect("the legacy document should be found");
                assert_eq!(document.revision, 7);
                assert_eq!(document.payload, "legacy-payload");
                Err(diesel::result::Error::RollbackTransaction)
            })
            .await;
        assert!(
            matches!(result, Err(diesel::result::Error::RollbackTransaction)),
            "unexpected lookup result: {result:?}"
        );
    }

    #[cfg(feature = "sqlite")]
    #[actix_rt::test]
    async fn blocked_appends_do_not_wait_for_the_write_lock() {
        use diesel_async::SimpleAsyncConnection;

        use crate::database::account_session;
        use crate::models::{AccountSession, EncryptedSync};

        let (database, mut writer) = crate::database::TestFileDatabase::new().await;
        writer
            .batch_execute(
                "INSERT INTO account (id, name_hash, password_hash, oidc_sub) \
                 VALUES ('account-a', 'hash-a', NULL, NULL);",
            )
            .await
            .unwrap();
        super::create(
            &mut writer,
            &EncryptedSync {
                account_id: "account-a".to_owned(),
                collection: "history".to_owned(),
                revision: 1,
                payload: "full".to_owned(),
            },
        )
        .await
        .unwrap();
        // A session of an older client that has never read appended changes.
        account_session::create(
            &mut writer,
            &AccountSession {
                id: "old-client".to_owned(),
                account_id: "account-a".to_owned(),
                device_id: "old-device".to_owned(),
                encrypted_device_info: None,
                created_at: 100,
                last_active_at: 100,
                expires_at: 500_000,
                revoked_at: None,
                legacy: false,
                generation: 0,
                pending_pairing: false,
            },
        )
        .await
        .unwrap();
        let mut reader = database.connect_without_waiting().await;
        // Stale revisions keep the conflict response older clients expect.
        let stale = super::append_change(
            &mut reader,
            "account-a",
            "history",
            0,
            "change",
            usize::MAX,
            None,
            1_000,
        )
        .await
        .unwrap();
        assert_eq!(stale, super::SaveResult::Conflict);

        writer.batch_execute("BEGIN IMMEDIATE;").await.unwrap();
        let result = super::append_change(
            &mut reader,
            "account-a",
            "history",
            1,
            "change",
            usize::MAX,
            None,
            1_000,
        )
        .await
        .unwrap();
        assert_eq!(result, super::SaveResult::FullUploadRequired);
    }

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
