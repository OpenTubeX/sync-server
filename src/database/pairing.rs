use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};

use crate::DbConnection;
use crate::database::DbError;
use crate::models::PairingSession;
use crate::schema::pairing_session::dsl::{
    account_id, approving_device_id, encrypted_payload, expires_at, id, pairing_session,
    recipient_device_id, recipient_device_name, recipient_public_key, recipient_token_hash,
    version,
};

const MAX_ACTIVE_SESSIONS: i64 = 10_000;
const MAX_ACTIVE_SESSIONS_PER_ACCOUNT: i64 = 5;

#[derive(Debug, PartialEq, Eq)]
pub enum CreateResult {
    Created,
    Duplicate,
    LimitExceeded,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimResult {
    Claimed(Box<PairingSession>),
    Conflict,
    LimitExceeded,
}

pub async fn create(
    conn: &mut DbConnection,
    session: &PairingSession,
    now: i64,
) -> Result<CreateResult, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            diesel::delete(pairing_session.filter(expires_at.le(now)))
                .execute(conn)
                .await?;

            let active = pairing_session.count().get_result::<i64>(conn).await?;
            if active >= MAX_ACTIVE_SESSIONS {
                return Ok(CreateResult::LimitExceeded);
            }

            match diesel::insert_into(pairing_session)
                .values(session)
                .execute(conn)
                .await
            {
                Ok(_) => Ok(CreateResult::Created),
                Err(diesel::result::Error::DatabaseError(
                    diesel::result::DatabaseErrorKind::UniqueViolation,
                    _,
                )) => Ok(CreateResult::Duplicate),
                Err(error) => Err(error),
            }
        })
    })
    .await
}

pub async fn claim(
    conn: &mut DbConnection,
    owner_id: &str,
    request: &PairingSession,
    now: i64,
) -> Result<ClaimResult, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            use crate::schema::account;

            // Serialize the per-account limit across workers and replicas.
            diesel::update(account::table.filter(account::id.eq(owner_id)))
                .set(account::id.eq(account::id))
                .execute(conn)
                .await?;
            diesel::delete(pairing_session.filter(expires_at.le(now)))
                .execute(conn)
                .await?;

            let active = pairing_session
                .filter(account_id.eq(owner_id))
                .count()
                .get_result::<i64>(conn)
                .await?;
            if active >= MAX_ACTIVE_SESSIONS_PER_ACCOUNT {
                return Ok(ClaimResult::LimitExceeded);
            }

            let claimed = diesel::update(
                pairing_session
                    .filter(id.eq(&request.id))
                    .filter(version.eq(request.version))
                    .filter(account_id.is_null())
                    .filter(recipient_public_key.eq(&request.recipient_public_key))
                    .filter(recipient_device_id.eq(&request.recipient_device_id))
                    .filter(recipient_device_name.eq(&request.recipient_device_name))
                    .filter(expires_at.gt(now))
                    .filter(encrypted_payload.is_null()),
            )
            .set(account_id.eq(owner_id))
            .returning(PairingSession::as_returning())
            .get_result(conn)
            .await
            .optional()?;

            Ok(match claimed {
                Some(session) => ClaimResult::Claimed(Box::new(session)),
                None => ClaimResult::Conflict,
            })
        })
    })
    .await
}

pub async fn get(
    conn: &mut DbConnection,
    session_id: &str,
    token_hash: &str,
    now: i64,
) -> Result<Option<PairingSession>, DbError> {
    pairing_session
        .filter(id.eq(session_id))
        .filter(version.eq(1))
        .filter(recipient_token_hash.eq(token_hash))
        .filter(expires_at.gt(now))
        .select(PairingSession::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn approve(
    conn: &mut DbConnection,
    owner_id: &str,
    session_id: &str,
    device_id: &str,
    payload: &str,
    now: i64,
) -> Result<bool, DbError> {
    let updated = diesel::update(
        pairing_session
            .filter(id.eq(session_id))
            .filter(version.eq(1))
            .filter(account_id.eq(owner_id))
            .filter(expires_at.gt(now))
            .filter(encrypted_payload.is_null()),
    )
    .set((
        approving_device_id.eq(device_id),
        encrypted_payload.eq(payload),
    ))
    .execute(conn)
    .await?;
    if updated == 1 {
        return Ok(true);
    }

    pairing_session
        .filter(id.eq(session_id))
        .filter(version.eq(1))
        .filter(account_id.eq(owner_id))
        .filter(approving_device_id.eq(device_id))
        .filter(encrypted_payload.eq(payload))
        .filter(expires_at.gt(now))
        .select(id)
        .first::<String>(conn)
        .await
        .optional()
        .map(|session| session.is_some())
}

pub async fn consume(
    conn: &mut DbConnection,
    session_id: &str,
    token_hash: &str,
    now: i64,
) -> Result<Option<PairingSession>, DbError> {
    diesel::delete(
        pairing_session
            .filter(id.eq(session_id))
            .filter(version.eq(1))
            .filter(recipient_token_hash.eq(token_hash))
            .filter(expires_at.gt(now))
            .filter(encrypted_payload.is_not_null()),
    )
    .returning(PairingSession::as_returning())
    .get_result(conn)
    .await
    .optional()
}

pub async fn cancel(
    conn: &mut DbConnection,
    session_id: &str,
    token_hash: &str,
) -> Result<bool, DbError> {
    let deleted = diesel::delete(
        pairing_session
            .filter(id.eq(session_id))
            .filter(version.eq(1))
            .filter(recipient_token_hash.eq(token_hash)),
    )
    .execute(conn)
    .await?;
    Ok(deleted == 1)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use diesel::connection::SimpleConnection;
    use diesel_async::AsyncConnection;
    use diesel_migrations::MigrationHarness;

    use super::{ClaimResult, CreateResult, approve, cancel, claim, consume, create, get};
    use crate::models::PairingSession;
    use crate::{DbConnection, MIGRATIONS};

    async fn connection() -> DbConnection {
        let mut conn = DbConnection::establish(":memory:").await.unwrap();
        conn.spawn_blocking(|conn| {
            conn.run_pending_migrations(MIGRATIONS).unwrap();
            conn.batch_execute(
                "PRAGMA foreign_keys = ON; \
                 INSERT INTO account (id, name_hash, password_hash, oidc_sub) \
                 VALUES ('account-a', 'hash-a', 'password', NULL); \
                 INSERT INTO account (id, name_hash, password_hash, oidc_sub) \
                 VALUES ('account-b', 'hash-b', 'password', NULL);",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        conn
    }

    fn session(id: &str, expires_at: i64) -> PairingSession {
        PairingSession {
            id: id.to_owned(),
            version: 1,
            account_id: None,
            recipient_public_key: "public-key".to_owned(),
            recipient_device_id: "recipient-device".to_owned(),
            recipient_device_name: "Laptop".to_owned(),
            recipient_token_hash: format!("token-{id}"),
            approving_device_id: None,
            encrypted_payload: None,
            expires_at,
        }
    }

    #[actix_rt::test]
    async fn sessions_require_the_recipient_token_and_are_single_use() {
        let mut conn = connection().await;
        let request = session("session", 1_000);
        assert_eq!(
            create(&mut conn, &request, 100).await.unwrap(),
            CreateResult::Created
        );
        assert_eq!(
            create(&mut conn, &request, 100).await.unwrap(),
            CreateResult::Duplicate
        );
        assert!(
            get(&mut conn, "session", "wrong-token", 100)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            get(&mut conn, "session", "token-session", 100)
                .await
                .unwrap()
                .is_some()
        );

        assert!(matches!(
            claim(&mut conn, "account-a", &request, 100).await.unwrap(),
            ClaimResult::Claimed(_)
        ));
        assert_eq!(
            claim(&mut conn, "account-b", &request, 100).await.unwrap(),
            ClaimResult::Conflict
        );
        assert!(
            !approve(&mut conn, "account-b", "session", "device", "payload", 100)
                .await
                .unwrap()
        );
        assert!(
            approve(&mut conn, "account-a", "session", "device", "payload", 100)
                .await
                .unwrap()
        );
        assert!(
            approve(&mut conn, "account-a", "session", "device", "payload", 100)
                .await
                .unwrap()
        );
        assert!(
            !approve(
                &mut conn,
                "account-a",
                "session",
                "device",
                "replacement",
                100
            )
            .await
            .unwrap()
        );

        assert!(
            consume(&mut conn, "session", "wrong-token", 100)
                .await
                .unwrap()
                .is_none()
        );
        let payload = consume(&mut conn, "session", "token-session", 100)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(payload.encrypted_payload.as_deref(), Some("payload"));
        assert!(
            consume(&mut conn, "session", "token-session", 100)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[actix_rt::test]
    async fn claims_match_the_request_and_expired_sessions_are_unusable() {
        let mut conn = connection().await;
        let request = session("session", 200);
        assert_eq!(
            create(&mut conn, &request, 100).await.unwrap(),
            CreateResult::Created
        );
        let mut changed = request.clone();
        changed.recipient_device_name = "Changed".to_owned();
        assert_eq!(
            claim(&mut conn, "account-a", &changed, 100).await.unwrap(),
            ClaimResult::Conflict
        );
        assert!(
            get(&mut conn, "session", "token-session", 200)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            claim(&mut conn, "account-a", &request, 200).await.unwrap(),
            ClaimResult::Conflict
        );
    }

    #[actix_rt::test]
    async fn active_claims_are_bounded_per_account() {
        let mut conn = connection().await;
        for index in 0..5 {
            let request = session(&format!("session-{index}"), 1_000);
            assert_eq!(
                create(&mut conn, &request, 100).await.unwrap(),
                CreateResult::Created
            );
            assert!(matches!(
                claim(&mut conn, "account-a", &request, 100).await.unwrap(),
                ClaimResult::Claimed(_)
            ));
        }

        let excess = session("session-5", 1_000);
        assert_eq!(
            create(&mut conn, &excess, 100).await.unwrap(),
            CreateResult::Created
        );
        assert_eq!(
            claim(&mut conn, "account-a", &excess, 100).await.unwrap(),
            ClaimResult::LimitExceeded
        );
        assert!(matches!(
            claim(&mut conn, "account-b", &excess, 100).await.unwrap(),
            ClaimResult::Claimed(_)
        ));
    }

    #[actix_rt::test]
    async fn recipient_can_cancel_a_pending_session() {
        let mut conn = connection().await;
        let request = session("session", 1_000);
        create(&mut conn, &request, 100).await.unwrap();
        assert!(!cancel(&mut conn, "session", "wrong-token").await.unwrap());
        assert!(cancel(&mut conn, "session", "token-session").await.unwrap());
    }
}
