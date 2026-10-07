use diesel::connection::SimpleConnection;
use diesel_async::AsyncConnection;
use diesel_migrations::MigrationHarness;

use super::{playlist, subscription_groups};
use crate::models::SubscriptionGroup;
use crate::{DbConnection, MIGRATIONS};

async fn connection() -> DbConnection {
    let mut conn = DbConnection::establish(":memory:").await.unwrap();
    conn.spawn_blocking(|conn| {
        conn.run_pending_migrations(MIGRATIONS).unwrap();
        conn.batch_execute(
            "PRAGMA foreign_keys = ON;
             INSERT INTO account (id, name_hash, password_hash)
               VALUES ('owner', 'owner-hash', 'password'), ('other', 'other-hash', 'password');
             INSERT INTO channel (id, name, verified) VALUES ('channel', 'Channel', FALSE);
             INSERT INTO video (id, title, upload_date, thumbnail_url, duration, uploader_id)
               VALUES ('video', 'Video', 0, 'https://i.ytimg.com/vi/video/default.jpg', 60, 'channel');
             INSERT INTO playlist (id, account_id, title, description)
               VALUES ('favorites', 'owner', 'Favorites', ''), ('favorites', 'other', 'Favorites', '');
             INSERT INTO playlist_video_member (account_id, playlist_id, video_id)
               VALUES ('owner', 'favorites', 'video'), ('other', 'favorites', 'video');
             INSERT INTO subscription_group (id, account_id, title)
               VALUES ('group', 'owner', 'Original');
             INSERT INTO subscription_group_member (subscription_group_id, channel_id)
               VALUES ('group', 'channel');",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    conn
}

#[actix_rt::test]
async fn deleting_a_playlist_preserves_other_accounts_with_the_same_id() {
    let mut conn = connection().await;
    playlist::delete_playlist_by_id(&mut conn, "favorites", "owner")
        .await
        .unwrap();

    assert!(
        playlist::get_playlist_by_id(&mut conn, "favorites", "owner")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        playlist::get_playlist_video_count(&mut conn, "favorites", "owner")
            .await
            .unwrap(),
        0
    );
    let (_, videos) = playlist::get_playlist_by_id_with_videos(&mut conn, "favorites", "other")
        .await
        .unwrap()
        .expect("the other account's playlist must remain");
    assert_eq!(videos.len(), 1);
}

#[actix_rt::test]
async fn updating_a_group_requires_ownership_and_preserves_members() {
    let mut conn = connection().await;
    let result = subscription_groups::update_existing_subscription_group(
        &mut conn,
        SubscriptionGroup {
            id: "group".into(),
            account_id: "other".into(),
            title: "Stolen".into(),
        },
    )
    .await;
    assert!(matches!(result, Err(diesel::result::Error::NotFound)));

    let group = subscription_groups::update_existing_subscription_group(
        &mut conn,
        SubscriptionGroup {
            id: "group".into(),
            account_id: "owner".into(),
            title: "Renamed".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(group.title, "Renamed");
    assert_eq!(group.account_id, "owner");
    let groups = subscription_groups::get_subscription_groups_by_account_id(&mut conn, "owner")
        .await
        .unwrap();
    assert_eq!(groups[0].1.len(), 1);
    assert!(
        subscription_groups::get_subscription_groups_by_account_id(&mut conn, "other")
            .await
            .unwrap()
            .is_empty()
    );
}

#[actix_rt::test]
async fn deleting_a_group_only_removes_its_owners_members() {
    let mut conn = connection().await;
    subscription_groups::delete_subscription_group_by_id(&mut conn, "group", "other")
        .await
        .unwrap();
    let groups = subscription_groups::get_subscription_groups_by_account_id(&mut conn, "owner")
        .await
        .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].1.len(), 1);

    subscription_groups::delete_subscription_group_by_id(&mut conn, "group", "owner")
        .await
        .unwrap();
    assert!(
        subscription_groups::get_subscription_groups_by_account_id(&mut conn, "owner")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        subscription_groups::get_subscription_group_channels_by_id(&mut conn, "group")
            .await
            .unwrap()
            .is_empty()
    );
}
