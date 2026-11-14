use crate::schema::sync_event::dsl::*;
use crate::{DbConnection, database::DbError};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

pub const MAX_EVENT_BYTES: usize = 256 * 1024;
const DAY: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Queryable, Selectable, Insertable, serde::Serialize, utoipa::ToSchema)]
#[diesel(table_name = crate::schema::sync_event)]
pub struct SyncEvent {
    pub id: String,
    #[serde(skip_serializing)]
    pub account_id: String,
    pub recipient: String,
    pub payload: String,
    pub created_at: i64,
    pub expires_at: i64,
}

pub async fn append(
    conn: &mut DbConnection,
    owner: &str,
    target: &str,
    ciphertext: &str,
    now: i64,
) -> Result<(), DbError> {
    diesel::delete(
        sync_event
            .filter(account_id.eq(owner))
            .filter(expires_at.le(now)),
    )
    .execute(conn)
    .await?;
    if !target.is_empty() {
        let pending: i64 = sync_event
            .filter(account_id.eq(owner))
            .filter(recipient.ne(""))
            .count()
            .get_result(conn)
            .await?;
        if pending >= 100 {
            return Err(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::CheckViolation,
                Box::new("Account device request queue is full".to_owned()),
            ));
        }
    }
    let retained: Vec<String> = sync_event
        .filter(account_id.eq(owner))
        .filter(recipient.eq(target))
        .order((created_at.desc(), id.desc()))
        .offset(99)
        .select(id)
        .load(conn)
        .await?;
    diesel::delete(
        sync_event
            .filter(account_id.eq(owner))
            .filter(id.eq_any(retained)),
    )
    .execute(conn)
    .await?;
    diesel::insert_into(sync_event)
        .values(SyncEvent {
            id: uuid::Uuid::now_v7().to_string(),
            account_id: owner.into(),
            recipient: target.into(),
            payload: ciphertext.into(),
            created_at: now,
            expires_at: now + if target.is_empty() { 30 * DAY } else { DAY },
        })
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn list(
    conn: &mut DbConnection,
    owner: &str,
    target: &str,
    since: &str,
    now: i64,
) -> Result<Vec<SyncEvent>, DbError> {
    // Filter broadcasts here, so already processed payloads are not loaded.
    sync_event
        .filter(account_id.eq(owner))
        .filter(recipient.eq(target).or(recipient.eq("").and(id.gt(since))))
        .filter(expires_at.gt(now))
        .order((created_at.desc(), id.desc()))
        .limit(200)
        .select(SyncEvent::as_select())
        .load(conn)
        .await
}

pub async fn acknowledge(
    conn: &mut DbConnection,
    owner: &str,
    target: &str,
    event: &str,
) -> Result<(), DbError> {
    if target.is_empty() {
        return Ok(());
    }
    diesel::delete(
        sync_event
            .filter(account_id.eq(owner))
            .filter(recipient.eq(target))
            .filter(id.eq(event)),
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn event_ids(
    conn: &mut DbConnection,
    owner: &str,
    target: &str,
    now: i64,
) -> Result<Vec<String>, DbError> {
    sync_event
        .filter(account_id.eq(owner))
        .filter(recipient.eq(target).or(recipient.eq("")))
        .filter(expires_at.gt(now))
        .order(id.asc())
        .select(id)
        .load(conn)
        .await
}

pub async fn delete_expired(conn: &mut DbConnection, now: i64) -> Result<usize, DbError> {
    diesel::delete(sync_event.filter(expires_at.le(now)))
        .execute(conn)
        .await
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::{
        MIGRATIONS,
        database::encrypted_sync::{SaveResult, save},
    };
    use diesel::connection::SimpleConnection;
    use diesel_async::{
        AsyncConnection,
        pooled_connection::{AsyncDieselConnectionManager, bb8::Pool},
    };
    use diesel_migrations::MigrationHarness;

    #[actix_rt::test]
    async fn events_are_atomic_bounded_expiring_and_scoped_to_account_and_recipient() {
        let pool = Pool::builder()
            .max_size(1)
            .build(AsyncDieselConnectionManager::<DbConnection>::new(
                ":memory:",
            ))
            .await
            .unwrap();
        let mut conn = pool.get().await.unwrap();
        conn.spawn_blocking(|conn| {
            conn.run_pending_migrations(MIGRATIONS).unwrap();
            conn.batch_execute(
                "INSERT INTO account (id, name_hash) VALUES ('a', 'a'), ('b', 'b');",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let now = 1000;
        assert_eq!(
            save(
                &mut conn,
                "a",
                "settings",
                0,
                "settings",
                1000000,
                Some(("activity", now))
            )
            .await
            .unwrap(),
            SaveResult::Saved
        );
        assert_eq!(
            save(
                &mut conn,
                "a",
                "settings",
                0,
                "conflict",
                1000000,
                Some(("must not exist", now))
            )
            .await
            .unwrap(),
            SaveResult::Conflict
        );
        assert_eq!(
            list(&mut conn, "a", "phone", "", now).await.unwrap().len(),
            1
        );
        assert!(
            list(&mut conn, "b", "phone", "", now)
                .await
                .unwrap()
                .is_empty()
        );
        append(&mut conn, "a", "phone", "video", now).await.unwrap();
        let events = list(&mut conn, "a", "phone", "", now).await.unwrap();
        assert_eq!(events.len(), 2);
        let message = events
            .iter()
            .find(|event| event.recipient == "phone")
            .unwrap();
        acknowledge(&mut conn, "b", "phone", &message.id)
            .await
            .unwrap();
        acknowledge(&mut conn, "a", "laptop", &message.id)
            .await
            .unwrap();
        assert_eq!(
            list(&mut conn, "a", "phone", "", now).await.unwrap().len(),
            2
        );
        assert_eq!(
            list(&mut conn, "a", "laptop", "", now).await.unwrap().len(),
            1
        );
        assert_eq!(
            list(&mut conn, "a", "phone", "", now + DAY)
                .await
                .unwrap()
                .len(),
            1
        );
        acknowledge(&mut conn, "a", "phone", &message.id)
            .await
            .unwrap();
        assert_eq!(
            list(&mut conn, "a", "phone", "", now).await.unwrap().len(),
            1
        );
        for offset in 1..105 {
            append(&mut conn, "a", "", "activity", now + offset)
                .await
                .unwrap();
        }
        assert_eq!(
            list(&mut conn, "a", "phone", "", now + 105)
                .await
                .unwrap()
                .len(),
            100
        );
        assert!(
            list(&mut conn, "a", "phone", "", now + 31 * DAY)
                .await
                .unwrap()
                .is_empty()
        );
        // Queue capacity is per account, even when requests target different devices.
        for index in 0..100 {
            append(
                &mut conn,
                "a",
                &format!("device-{index}"),
                "video",
                now + 106,
            )
            .await
            .unwrap();
        }
        assert!(
            append(&mut conn, "a", "one-more-device", "video", now + 106)
                .await
                .is_err()
        );
        assert_eq!(
            list(&mut conn, "a", "device-0", "", now + 106)
                .await
                .unwrap()
                .len(),
            101
        );
        // Rolling back the enclosing transaction also rolls back its activity.
        let result: Result<(), diesel::result::Error> = conn
            .transaction(async |conn| {
                save(
                    conn,
                    "b",
                    "settings",
                    0,
                    "settings",
                    1000000,
                    Some(("rollback", now)),
                )
                .await?;
                Err(diesel::result::Error::RollbackTransaction)
            })
            .await;
        assert!(result.is_err());
        assert!(
            list(&mut conn, "b", "phone", "", now)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            crate::database::encrypted_sync::get_full(&mut conn, "b", "settings")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Activity payloads are padded to tens of kilobytes, so SQLite stores
    /// most of each event in a chain of overflow pages, followed by the
    /// columns after `payload`. Every write with activity and every change
    /// poll must not walk those chains: on a slow disk that held the write
    /// lock for seconds per request.
    #[actix_rt::test]
    async fn writes_and_polls_do_not_read_stored_event_payloads() {
        use crate::database::encrypted_sync;
        use crate::models::EncryptedSync;
        use diesel_async::SimpleAsyncConnection;

        let (database, mut conn) = crate::database::TestFileDatabase::new().await;
        conn.batch_execute("INSERT INTO account (id, name_hash) VALUES ('a', 'a');")
            .await
            .unwrap();
        encrypted_sync::create(
            &mut conn,
            &EncryptedSync {
                account_id: "a".to_owned(),
                collection: "watchStats".to_owned(),
                revision: 1,
                payload: "stats".to_owned(),
            },
        )
        .await
        .unwrap();
        let padded = "E".repeat(87_610);
        let now = 10 * DAY;
        // Below the retention limit, so the next append deletes none of them.
        for index in 0..99 {
            append(&mut conn, "a", "", &padded, now - 1000 + index)
                .await
                .unwrap();
        }
        let newest = list(&mut conn, "a", "phone", "", now).await.unwrap()[0]
            .id
            .clone();
        conn.batch_execute("PRAGMA wal_checkpoint(TRUNCATE);")
            .await
            .unwrap();
        drop(conn);

        // Point every full overflow page of the stored payloads at a page
        // past the end of the file, so walking a chain fails as corrupt.
        let mut file = std::fs::read(&database.url).unwrap();
        let mut cut = 0;
        for page in file.chunks_mut(4096) {
            if page[4..].iter().all(|&byte| byte == b'E') {
                page[..4].copy_from_slice(&u32::MAX.to_be_bytes());
                cut += 1;
            }
        }
        assert!(cut >= 99 * 20, "only {cut} overflow pages found");
        std::fs::write(&database.url, file).unwrap();

        let mut conn = database.connect_without_waiting().await;
        assert_eq!(
            event_ids(&mut conn, "a", "phone", now).await.unwrap().len(),
            99
        );
        assert!(
            list(&mut conn, "a", "phone", &newest, now)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            encrypted_sync::save(
                &mut conn,
                "a",
                "watchStats",
                1,
                "new stats",
                usize::MAX,
                Some(("activity", now)),
            )
            .await
            .unwrap(),
            SaveResult::Saved
        );
        let events = list(&mut conn, "a", "phone", &newest, now).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload, "activity");
    }
}
