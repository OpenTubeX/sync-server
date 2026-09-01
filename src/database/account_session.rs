use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};

use crate::database::DbError;
use crate::models::AccountSession;
use crate::schema::account_session::dsl::{
    account_id, account_session, encrypted_device_info, expires_at, generation, id, last_active_at,
    pending_pairing, revoked_at,
};
use crate::{DbConnection, schema};

pub async fn create(
    conn: &mut DbConnection,
    session: &AccountSession,
) -> Result<AccountSession, DbError> {
    diesel::insert_into(account_session)
        .values(session)
        .returning(AccountSession::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_or_create(
    conn: &mut DbConnection,
    session: &AccountSession,
) -> Result<AccountSession, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            diesel::insert_into(account_session)
                .values(session)
                .on_conflict(id)
                .do_nothing()
                .execute(conn)
                .await?;

            account_session
                .filter(id.eq(&session.id))
                .filter(account_id.eq(&session.account_id))
                .filter(generation.eq(session.generation))
                .filter(expires_at.gt(session.created_at))
                .filter(revoked_at.is_null())
                .select(AccountSession::as_select())
                .first(conn)
                .await
        })
    })
    .await
}

pub async fn find_active(
    conn: &mut DbConnection,
    owner_id: &str,
    session_id: &str,
    current_generation: i64,
    now: i64,
) -> Result<Option<AccountSession>, DbError> {
    account_session
        .filter(id.eq(session_id))
        .filter(account_id.eq(owner_id))
        .filter(generation.eq(current_generation))
        .filter(pending_pairing.eq(false))
        .filter(expires_at.gt(now))
        .filter(revoked_at.is_null())
        .select(AccountSession::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn touch(
    conn: &mut DbConnection,
    session_id: &str,
    current_generation: i64,
    previous_activity: i64,
    now: i64,
) -> Result<bool, DbError> {
    if now.saturating_sub(previous_activity) < 5 * 60 * 1000 {
        return Ok(false);
    }

    diesel::update(
        account_session
            .filter(id.eq(session_id))
            .filter(generation.eq(current_generation))
            .filter(pending_pairing.eq(false))
            .filter(last_active_at.eq(previous_activity))
            .filter(expires_at.gt(now))
            .filter(revoked_at.is_null()),
    )
    .set(last_active_at.eq(now))
    .execute(conn)
    .await
    .map(|updated| updated == 1)
}

pub async fn list_active(
    conn: &mut DbConnection,
    owner_id: &str,
    current_generation: i64,
    now: i64,
) -> Result<Vec<AccountSession>, DbError> {
    diesel::delete(
        account_session
            .filter(account_id.eq(owner_id))
            .filter(expires_at.le(now)),
    )
    .execute(conn)
    .await?;

    account_session
        .filter(account_id.eq(owner_id))
        .filter(generation.eq(current_generation))
        .filter(pending_pairing.eq(false))
        .filter(expires_at.gt(now))
        .filter(revoked_at.is_null())
        .order(last_active_at.desc())
        .select(AccountSession::as_select())
        .load(conn)
        .await
}

pub async fn update_encrypted_info(
    conn: &mut DbConnection,
    owner_id: &str,
    session_id: &str,
    current_generation: i64,
    value: &str,
    now: i64,
) -> Result<bool, DbError> {
    diesel::update(
        account_session
            .filter(id.eq(session_id))
            .filter(account_id.eq(owner_id))
            .filter(generation.eq(current_generation))
            .filter(pending_pairing.eq(false))
            .filter(expires_at.gt(now))
            .filter(revoked_at.is_null()),
    )
    .set(encrypted_device_info.eq(value))
    .execute(conn)
    .await
    .map(|updated| updated == 1)
}

pub async fn revoke(
    conn: &mut DbConnection,
    owner_id: &str,
    session_id: &str,
    current_generation: i64,
    now: i64,
) -> Result<bool, DbError> {
    diesel::update(
        account_session
            .filter(id.eq(session_id))
            .filter(account_id.eq(owner_id))
            .filter(generation.eq(current_generation))
            .filter(pending_pairing.eq(false))
            .filter(revoked_at.is_null()),
    )
    .set(revoked_at.eq(now))
    .execute(conn)
    .await
    .map(|updated| updated == 1)
}

pub async fn change_password_and_revoke_others(
    conn: &mut DbConnection,
    owner_id: &str,
    replacement: &AccountSession,
    expected_password_hash: &str,
    new_password_hash: &str,
    now: i64,
) -> Result<bool, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            let updated = diesel::update(
                schema::account::table
                    .filter(schema::account::id.eq(owner_id))
                    .filter(schema::account::password_hash.eq(Some(expected_password_hash)))
                    .filter(
                        schema::account::session_generation
                            .eq(replacement.generation.saturating_sub(1)),
                    ),
            )
            .set((
                schema::account::password_hash.eq(Some(new_password_hash)),
                schema::account::legacy_tokens_enabled.eq(false),
                schema::account::session_generation.eq(replacement.generation),
            ))
            .execute(conn)
            .await?;
            if updated != 1 {
                return Ok(false);
            }
            diesel::insert_into(account_session)
                .values(replacement)
                .execute(conn)
                .await?;
            diesel::update(
                account_session
                    .filter(account_id.eq(owner_id))
                    .filter(id.ne(&replacement.id))
                    .filter(revoked_at.is_null()),
            )
            .set(revoked_at.eq(now))
            .execute(conn)
            .await?;
            Ok(true)
        })
    })
    .await
}

pub async fn delete_expired(conn: &mut DbConnection, now: i64) -> Result<usize, DbError> {
    diesel::delete(account_session.filter(expires_at.le(now)))
        .execute(conn)
        .await
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use diesel::connection::SimpleConnection;
    use diesel_async::AsyncConnection;
    use diesel_migrations::MigrationHarness;

    use super::{
        change_password_and_revoke_others, create, find_active, get_or_create, list_active, revoke,
        touch, update_encrypted_info,
    };
    use crate::database::account::find_account_by_id;
    use crate::models::AccountSession;
    use crate::{DbConnection, MIGRATIONS};

    async fn connection() -> DbConnection {
        let mut conn = DbConnection::establish(":memory:").await.unwrap();
        conn.spawn_blocking(|conn| {
            conn.run_pending_migrations(MIGRATIONS).unwrap();
            conn.batch_execute(
                "PRAGMA foreign_keys = ON; \
                 INSERT INTO account (id, name_hash, password_hash, oidc_sub) \
                 VALUES ('account-a', 'hash-a', 'old-password', NULL);",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        conn
    }

    fn session(id: &str, last_active_at: i64, expires_at: i64) -> AccountSession {
        AccountSession {
            id: id.to_owned(),
            account_id: "account-a".to_owned(),
            device_id: format!("device-{id}"),
            encrypted_device_info: None,
            created_at: 100,
            last_active_at,
            expires_at,
            revoked_at: None,
            legacy: false,
            generation: 0,
            pending_pairing: false,
        }
    }

    #[actix_rt::test]
    async fn active_sessions_can_be_listed_updated_touched_and_revoked() {
        let mut conn = connection().await;
        create(&mut conn, &session("current", 100, 500_000))
            .await
            .unwrap();
        create(&mut conn, &session("expired", 100, 500))
            .await
            .unwrap();

        assert_eq!(
            list_active(&mut conn, "account-a", 0, 500)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            update_encrypted_info(&mut conn, "account-a", "current", 0, "ciphertext", 500)
                .await
                .unwrap()
        );
        assert!(!touch(&mut conn, "current", 0, 100, 500).await.unwrap());
        assert!(touch(&mut conn, "current", 0, 100, 400_000).await.unwrap());

        let active = find_active(&mut conn, "account-a", "current", 0, 500)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.encrypted_device_info.as_deref(), Some("ciphertext"));
        assert_eq!(active.last_active_at, 400_000);
        assert!(
            revoke(&mut conn, "account-a", "current", 0, 450_000)
                .await
                .unwrap()
        );
        assert!(
            find_active(&mut conn, "account-a", "current", 0, 500)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            get_or_create(&mut conn, &session("current", 450_000, 500_000)).await,
            Err(diesel::result::Error::NotFound)
        ));
    }

    #[actix_rt::test]
    async fn password_change_rotates_the_requesting_session() {
        let mut conn = connection().await;
        create(&mut conn, &session("current", 100, 2_000))
            .await
            .unwrap();
        create(&mut conn, &session("other", 100, 2_000))
            .await
            .unwrap();
        let mut replacement = session("replacement", 500, 2_000);
        replacement.generation = 1;

        assert!(
            change_password_and_revoke_others(
                &mut conn,
                "account-a",
                &replacement,
                "old-password",
                "new-password",
                500,
            )
            .await
            .unwrap()
        );

        let account = find_account_by_id(&mut conn, "account-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account.password_hash.as_deref(), Some("new-password"));
        assert!(!account.legacy_tokens_enabled);
        assert_eq!(account.session_generation, 1);
        let sessions = list_active(&mut conn, "account-a", 1, 500).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "replacement");

        let late_login = session("late-login", 500, 2_000);
        create(&mut conn, &late_login).await.unwrap();
        assert!(
            find_active(&mut conn, "account-a", "late-login", 1, 500)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !change_password_and_revoke_others(
                &mut conn,
                "account-a",
                &replacement,
                "old-password",
                "racing-password",
                500,
            )
            .await
            .unwrap()
        );
    }
}
