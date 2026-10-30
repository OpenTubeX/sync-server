pub mod account;
pub mod account_session;
pub mod channel;
pub mod encrypted_sync;
pub mod pairing;
pub mod playlist;
pub mod playlist_bookmark;
pub mod public_playlist;
pub mod quota;
pub mod subscription;
pub mod subscription_groups;
pub mod video;
pub mod watch_history;

type DbError = diesel::result::Error;

#[cfg(all(test, feature = "sqlite"))]
mod ownership_tests;

pub mod sync_event;

/// A migrated WAL database file, for tests that need a second connection to
/// hold the write lock.
#[cfg(all(test, feature = "sqlite"))]
pub struct TestFileDatabase {
    url: String,
}

#[cfg(all(test, feature = "sqlite"))]
impl TestFileDatabase {
    pub async fn new() -> (Self, crate::DbConnection) {
        use diesel::connection::SimpleConnection;
        use diesel_async::AsyncConnection;
        use diesel_migrations::MigrationHarness;

        let path = std::env::temp_dir().join(format!(
            "opentubex-sync-test-{}.sqlite",
            uuid::Uuid::now_v7()
        ));
        let database = Self {
            url: path.to_str().unwrap().to_owned(),
        };
        let mut conn = crate::DbConnection::establish(&database.url).await.unwrap();
        conn.spawn_blocking(|conn| {
            conn.batch_execute("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
            conn.run_pending_migrations(crate::MIGRATIONS).unwrap();
            Ok(())
        })
        .await
        .unwrap();
        (database, conn)
    }

    /// A connection that fails at once instead of waiting for the write lock.
    pub async fn connect_without_waiting(&self) -> crate::DbConnection {
        use diesel_async::{AsyncConnection, SimpleAsyncConnection};

        let mut conn = crate::DbConnection::establish(&self.url).await.unwrap();
        conn.batch_execute("PRAGMA busy_timeout = 0;")
            .await
            .unwrap();
        conn
    }
}

#[cfg(all(test, feature = "sqlite"))]
impl Drop for TestFileDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.url));
        }
    }
}
