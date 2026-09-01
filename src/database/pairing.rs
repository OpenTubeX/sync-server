use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};

use crate::DbConnection;
use crate::database::DbError;
use crate::models::{AccountSession, PairingSession};
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
    Claimed {
        pairing: Box<PairingSession>,
        account_session: Box<AccountSession>,
    },
    Conflict,
    LimitExceeded,
}

pub async fn delete_expired(conn: &mut DbConnection, now: i64) -> Result<usize, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            let expired_ids = pairing_session
                .filter(expires_at.le(now))
                .select(id)
                .load::<String>(conn)
                .await?;
            if expired_ids.is_empty() {
                return Ok(0);
            }
            diesel::delete(
                crate::schema::account_session::table
                    .filter(crate::schema::account_session::id.eq_any(&expired_ids))
                    .filter(crate::schema::account_session::pending_pairing.eq(true)),
            )
            .execute(conn)
            .await?;
            diesel::delete(pairing_session.filter(id.eq_any(expired_ids)))
                .execute(conn)
                .await
        })
    })
    .await
}

pub async fn create(
    conn: &mut DbConnection,
    session: &PairingSession,
    now: i64,
) -> Result<CreateResult, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            delete_expired(conn, now).await?;

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
    candidate_session: &AccountSession,
    now: i64,
) -> Result<ClaimResult, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            use crate::schema::account;

            // Serialize the per-account limit across workers and replicas.
            let account_locked = diesel::update(
                account::table
                    .filter(account::id.eq(owner_id))
                    .filter(account::session_generation.eq(candidate_session.generation)),
            )
            .set(account::id.eq(account::id))
            .execute(conn)
            .await?;
            if account_locked != 1 {
                return Ok(ClaimResult::Conflict);
            }
            delete_expired(conn, now).await?;

            let existing = pairing_session
                .filter(id.eq(&request.id))
                .filter(version.eq(request.version))
                .filter(account_id.eq(owner_id))
                .filter(recipient_public_key.eq(&request.recipient_public_key))
                .filter(recipient_device_id.eq(&request.recipient_device_id))
                .filter(recipient_device_name.eq(&request.recipient_device_name))
                .filter(expires_at.gt(now))
                .filter(encrypted_payload.is_null())
                .select(PairingSession::as_select())
                .first(conn)
                .await
                .optional()?;
            if let Some(session) = existing {
                let account_session =
                    crate::database::account_session::get_or_create(conn, candidate_session)
                        .await?;
                return Ok(ClaimResult::Claimed {
                    pairing: Box::new(session),
                    account_session: Box::new(account_session),
                });
            }

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

            let Some(pairing) = claimed else {
                return Ok(ClaimResult::Conflict);
            };
            let account_session =
                crate::database::account_session::get_or_create(conn, candidate_session).await?;
            Ok(ClaimResult::Claimed {
                pairing: Box::new(pairing),
                account_session: Box::new(account_session),
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
    conn.transaction(|conn| {
        Box::pin(async move {
            let consumed = diesel::delete(
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
            .optional()?;
            let Some(session) = consumed else {
                return Ok(None);
            };
            let activated = diesel::update(
                crate::schema::account_session::table
                    .filter(crate::schema::account_session::id.eq(session_id))
                    .filter(crate::schema::account_session::pending_pairing.eq(true)),
            )
            .set(crate::schema::account_session::pending_pairing.eq(false))
            .execute(conn)
            .await?;
            if activated != 1 {
                return Err(diesel::result::Error::NotFound);
            }
            Ok(Some(session))
        })
    })
    .await
}

pub async fn cancel(
    conn: &mut DbConnection,
    session_id: &str,
    token_hash: &str,
) -> Result<bool, DbError> {
    conn.transaction(|conn| {
        Box::pin(async move {
            let deleted = diesel::delete(
                pairing_session
                    .filter(id.eq(session_id))
                    .filter(version.eq(1))
                    .filter(recipient_token_hash.eq(token_hash)),
            )
            .execute(conn)
            .await?;
            if deleted != 1 {
                return Ok(false);
            }
            diesel::delete(
                crate::schema::account_session::table
                    .filter(crate::schema::account_session::id.eq(session_id))
                    .filter(crate::schema::account_session::pending_pairing.eq(true)),
            )
            .execute(conn)
            .await?;
            Ok(true)
        })
    })
    .await
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use diesel::connection::SimpleConnection;
    use diesel_async::AsyncConnection;
    use diesel_migrations::MigrationHarness;

    use super::{
        ClaimResult, CreateResult, approve, cancel, claim, consume, create, delete_expired, get,
    };
    use crate::models::{AccountSession, PairingSession};
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

    fn account_session(request: &PairingSession, owner_id: &str) -> AccountSession {
        AccountSession {
            id: request.id.clone(),
            account_id: owner_id.to_owned(),
            device_id: request.recipient_device_id.clone(),
            encrypted_device_info: Some("ciphertext".to_owned()),
            created_at: 100,
            last_active_at: 100,
            expires_at: 1_000_000,
            revoked_at: None,
            legacy: false,
            generation: 0,
            pending_pairing: true,
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
            claim(
                &mut conn,
                "account-a",
                &request,
                &account_session(&request, "account-a"),
                100,
            )
            .await
            .unwrap(),
            ClaimResult::Claimed { .. }
        ));
        assert_eq!(
            claim(
                &mut conn,
                "account-b",
                &request,
                &account_session(&request, "account-b"),
                100,
            )
            .await
            .unwrap(),
            ClaimResult::Conflict
        );
        assert!(
            crate::database::account_session::list_active(&mut conn, "account-a", 0, 100)
                .await
                .unwrap()
                .is_empty()
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
        assert_eq!(
            crate::database::account_session::list_active(&mut conn, "account-a", 0, 100)
                .await
                .unwrap()
                .len(),
            1
        );
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
            claim(
                &mut conn,
                "account-a",
                &changed,
                &account_session(&changed, "account-a"),
                100,
            )
            .await
            .unwrap(),
            ClaimResult::Conflict
        );
        assert!(
            get(&mut conn, "session", "token-session", 200)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(delete_expired(&mut conn, 200).await.unwrap(), 1);
        assert_eq!(
            claim(
                &mut conn,
                "account-a",
                &request,
                &account_session(&request, "account-a"),
                200,
            )
            .await
            .unwrap(),
            ClaimResult::Conflict
        );
        assert!(!cancel(&mut conn, "session", "token-session").await.unwrap());
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
                claim(
                    &mut conn,
                    "account-a",
                    &request,
                    &account_session(&request, "account-a"),
                    100,
                )
                .await
                .unwrap(),
                ClaimResult::Claimed { .. }
            ));
        }

        let retry = session("session-0", 1_000);
        assert!(matches!(
            claim(
                &mut conn,
                "account-a",
                &retry,
                &account_session(&retry, "account-a"),
                100,
            )
            .await
            .unwrap(),
            ClaimResult::Claimed { .. }
        ));

        let excess = session("session-5", 1_000);
        assert_eq!(
            create(&mut conn, &excess, 100).await.unwrap(),
            CreateResult::Created
        );
        assert_eq!(
            claim(
                &mut conn,
                "account-a",
                &excess,
                &account_session(&excess, "account-a"),
                100,
            )
            .await
            .unwrap(),
            ClaimResult::LimitExceeded
        );
        assert!(matches!(
            claim(
                &mut conn,
                "account-b",
                &excess,
                &account_session(&excess, "account-b"),
                100,
            )
            .await
            .unwrap(),
            ClaimResult::Claimed { .. }
        ));
    }

    #[actix_rt::test]
    async fn recipient_can_cancel_a_pending_session() {
        let mut conn = connection().await;
        let request = session("session", 1_000);
        create(&mut conn, &request, 100).await.unwrap();
        let provisional = account_session(&request, "account-a");
        assert!(matches!(
            claim(&mut conn, "account-a", &request, &provisional, 100)
                .await
                .unwrap(),
            ClaimResult::Claimed { .. }
        ));
        assert!(!cancel(&mut conn, "session", "wrong-token").await.unwrap());
        assert!(cancel(&mut conn, "session", "token-session").await.unwrap());
        crate::database::account_session::create(&mut conn, &provisional)
            .await
            .expect("cancelling must remove the provisional account session");
    }

    #[actix_rt::test]
    async fn expiration_removes_the_provisional_account_session() {
        let mut conn = connection().await;
        let request = session("session", 200);
        create(&mut conn, &request, 100).await.unwrap();
        let provisional = account_session(&request, "account-a");
        assert!(matches!(
            claim(&mut conn, "account-a", &request, &provisional, 100)
                .await
                .unwrap(),
            ClaimResult::Claimed { .. }
        ));

        assert_eq!(delete_expired(&mut conn, 200).await.unwrap(), 1);
        crate::database::account_session::create(&mut conn, &provisional)
            .await
            .expect("expiration must remove the provisional account session");
    }
}
