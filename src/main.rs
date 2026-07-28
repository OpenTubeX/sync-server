#[macro_use]
extern crate diesel;

use std::{io, sync::LazyLock};

// only used by the sqlite pre-migration backup
#[cfg(feature = "sqlite")]
use std::path::Path;

use actix_web::{App, HttpServer, middleware, web};
#[cfg(feature = "sqlite")]
use diesel::connection::SimpleConnection;
#[cfg(feature = "sqlite")]
use diesel_async::pooled_connection::ManagerConfig;
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, PoolError, bb8::Pool};
#[cfg(feature = "sqlite")]
use diesel_async::{AsyncConnection, SimpleAsyncConnection};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use log::error;
use utoipa::OpenApi;
use utoipa_actix_web::AppExt;
use utoipa_scalar::{Scalar, Servable};

use crate::{
    handlers::{
        ScopedHandler, channel_playback_speeds::ChannelPlaybackSpeedsHandler,
        encrypted_sync::EncryptedSyncHandler, health::HealthHandler,
        playlist_bookmarks::PlaylistBookmarksHandler, playlists::PlaylistsHandler,
        subscriptions::SubscriptionsHandler, user::UserHandler, watch_history::WatchHistoryHandler,
    },
    openapi::ApiDoc,
};

mod auth;
mod config;
mod database;
mod dto;
mod handlers;
mod models;
mod openapi;
mod rate_limit;
mod schema;
mod validation;

static CONFIG: LazyLock<config::Config> = LazyLock::new(|| match config::build_config() {
    Ok(c) => c,
    Err(e) => {
        error!("Failed to configure server: {e}");
        std::process::exit(1);
    }
});

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations/");

#[cfg(all(feature = "sqlite", feature = "postgres"))]
compile_error!("Sqlite and Postgres are mutually exclusive and cannot be enabled together");

#[cfg(feature = "sqlite")]
type DbConnection =
    diesel_async::sync_connection_wrapper::SyncConnectionWrapper<diesel::SqliteConnection>;
#[cfg(feature = "postgres")]
type DbConnection = diesel_async::AsyncPgConnection;

type DbPool = Pool<DbConnection>;
type WebData = web::Data<DbPool>;

#[actix_web::main]
async fn main() -> io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    // initialize DB pool outside `HttpServer::new` so that it is shared across all workers
    let pool = match initialize_db_pool(&CONFIG.database_url).await {
        Ok(pool) => pool,
        Err(err) => panic!("{}", err),
    };

    // run database migrations (must be done BEFORE the server is started!)
    run_migrations(
        &pool,
        &CONFIG.database_url,
        CONFIG.migration_approval.as_deref(),
    )
    .await;

    log::info!("starting HTTP server at http://localhost:8080");

    HttpServer::new(move || {
        let (app, generated_api) = App::new()
            .into_utoipa_app()
            // add DB pool handle to app data; enables use of `web::Data<DbPool>` extractor
            .app_data(web::Data::new(pool.clone()))
            .service(
                utoipa_actix_web::scope("/v1")
                    .service(UserHandler::get_service())
                    .service(ChannelPlaybackSpeedsHandler::get_service())
                    .service(SubscriptionsHandler::get_service())
                    .service(PlaylistsHandler::get_service())
                    .service(PlaylistBookmarksHandler::get_service())
                    .service(WatchHistoryHandler::get_service())
                    .service(EncryptedSyncHandler::get_service()),
            )
            .split_for_parts();

        // add additional meta and security info
        let mut api = ApiDoc::openapi();
        api.merge(generated_api);

        // docs service must be registered before health handler!
        app.service(Scalar::with_url("/docs", api))
            .service(HealthHandler::get_service())
            .wrap(middleware::Logger::default())
    })
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}

/// Initialize database connection pool based on `DATABASE_URL` environment variable.
///
/// See more: <https://docs.rs/diesel-async/latest/diesel_async/pooled_connection/index.html#modules>.
async fn initialize_db_pool(db_url: &str) -> Result<DbPool, PoolError> {
    #[cfg(feature = "sqlite")]
    let connection_manager = {
        let mut manager_config = ManagerConfig::default();
        manager_config.custom_setup = Box::new(|url| {
            let url = url.to_owned();
            Box::pin(async move {
                let mut conn = DbConnection::establish(&url).await?;
                // `foreign_keys` defaults to OFF in SQLite and must be enabled per
                // connection, otherwise none of the ON DELETE CASCADE constraints
                // fire and deleting an account orphans all of its rows.
                conn.batch_execute(
                    "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 30000; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;",
                )
                .await
                .map_err(|err| diesel::ConnectionError::BadConnection(err.to_string()))?;
                Ok(conn)
            })
        });
        AsyncDieselConnectionManager::<DbConnection>::new_with_config(db_url, manager_config)
    };

    #[cfg(feature = "postgres")]
    let connection_manager = AsyncDieselConnectionManager::<DbConnection>::new(db_url);

    Pool::builder().build(connection_manager).await
}

fn require_migration_approval(
    has_applied_migrations: bool,
    pending_versions: &[String],
    approval: Option<&str>,
) -> Result<(), String> {
    if !has_applied_migrations || pending_versions.is_empty() {
        return Ok(());
    }

    let required = pending_versions.join(",");
    if approval == Some(required.as_str()) {
        return Ok(());
    }

    Err(format!(
        "refusing to migrate an existing database; create and verify a backup, then set MIGRATION_APPROVAL={required} for this deployment"
    ))
}

#[cfg(feature = "sqlite")]
fn back_up_sqlite_before_migration(
    conn: &mut diesel::SqliteConnection,
    database_url: &str,
    latest_version: &str,
) {
    let backup_path = format!("{database_url}.pre-migration-{latest_version}");
    if Path::new(&backup_path).exists() {
        log::info!("using existing pre-migration backup at {backup_path}");
        return;
    }

    let escaped_path = backup_path.replace('\'', "''");
    conn.batch_execute(&format!("VACUUM INTO '{escaped_path}'"))
        .unwrap_or_else(|error| {
            panic!("failed to create migration backup at {backup_path}: {error}")
        });
    log::info!("created pre-migration backup at {backup_path}");
}

async fn run_migrations(pool: &DbPool, database_url: &str, approval: Option<&str>) {
    // https://github.com/diesel-rs/diesel_async/discussions/268
    //
    // An unwritable data directory shows up here as a pool timeout rather than a
    // permission error, which is confusing enough to be worth calling out.
    let conn = pool.get_owned().await.unwrap_or_else(|err| {
        panic!(
            "could not open the database at {database_url}: {err}. \
             If this is a timeout, check that the database and its directory are \
             writable by the user the server runs as (uid 10001 in the Docker image)."
        )
    });

    #[cfg(feature = "sqlite")]
    {
        let mut conn = conn;
        let database_url = database_url.to_owned();
        let approval = approval.map(str::to_owned);
        conn.spawn_blocking(move |conn| {
            let pending = conn.pending_migrations(MIGRATIONS).unwrap();
            let pending_versions = pending
                .iter()
                .map(|migration| migration.name().version().to_string())
                .collect::<Vec<_>>();
            let has_applied_migrations = !conn.applied_migrations().unwrap().is_empty();
            require_migration_approval(
                has_applied_migrations,
                &pending_versions,
                approval.as_deref(),
            )
            .unwrap_or_else(|message| panic!("{message}"));
            if has_applied_migrations && !pending_versions.is_empty() {
                back_up_sqlite_before_migration(
                    conn,
                    &database_url,
                    pending_versions.last().unwrap(),
                );
            }
            conn.run_migrations(&pending).unwrap();
            Ok(())
        })
        .await
        .unwrap();
    }

    #[cfg(feature = "postgres")]
    {
        // must be spawned blocking, otherwise this would raise 'can call blocking only when running on the multi-threaded runtime': see https://github.com/rwf2/Rocket/pull/2648
        let approval = approval.map(str::to_owned);
        actix_web::rt::task::spawn_blocking(move || {
            let mut harness = diesel_async::AsyncMigrationHarness::new(conn);
            let pending = harness.pending_migrations(MIGRATIONS).unwrap();
            let pending_versions = pending
                .iter()
                .map(|migration| migration.name().version().to_string())
                .collect::<Vec<_>>();
            let has_applied_migrations = !harness.applied_migrations().unwrap().is_empty();
            require_migration_approval(
                has_applied_migrations,
                &pending_versions,
                approval.as_deref(),
            )
            .unwrap_or_else(|message| panic!("{message}"));
            harness.run_migrations(&pending).unwrap();
        })
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::require_migration_approval;

    #[test]
    fn fresh_database_does_not_require_approval() {
        assert!(require_migration_approval(false, &["20260721".to_owned()], None).is_ok());
    }

    #[test]
    fn existing_database_requires_exact_pending_versions() {
        let pending = ["20260721".to_owned(), "20260722".to_owned()];
        assert!(require_migration_approval(true, &pending, None).is_err());
        assert!(require_migration_approval(true, &pending, Some("20260721")).is_err());
        assert!(require_migration_approval(true, &pending, Some("20260721,20260722")).is_ok());
    }
}
