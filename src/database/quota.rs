//! Per-account row quotas for the legacy plaintext tables.
//!
//! The encrypted sync path is bounded by `MAX_ENCRYPTED_SYNC_ACCOUNT_BYTES`, but
//! the plaintext tables had no limit at all, so a single account could grow the
//! database without bound (and sidestep the encrypted quota by using the legacy
//! endpoints instead).

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use crate::DbConnection;
use crate::database::DbError;

/// Maximum rows one account may hold in any single plaintext table.
///
/// Chosen to sit far above realistic library sizes while still bounding abuse.
pub const MAX_ROWS_PER_ACCOUNT: i64 = 50_000;

pub async fn count_subscriptions(conn: &mut DbConnection, owner_id: &str) -> Result<i64, DbError> {
    use crate::schema::subscription::dsl::*;

    subscription
        .filter(account_id.eq(owner_id))
        .count()
        .get_result(conn)
        .await
}

pub async fn count_watch_history(conn: &mut DbConnection, owner_id: &str) -> Result<i64, DbError> {
    use crate::schema::watch_history::dsl::*;

    watch_history
        .filter(account_id.eq(owner_id))
        .count()
        .get_result(conn)
        .await
}

pub async fn count_playlist_videos(
    conn: &mut DbConnection,
    owner_id: &str,
) -> Result<i64, DbError> {
    use crate::schema::playlist_video_member::dsl::*;

    playlist_video_member
        .filter(account_id.eq(owner_id))
        .count()
        .get_result(conn)
        .await
}

pub async fn count_playback_speeds(
    conn: &mut DbConnection,
    owner_id: &str,
) -> Result<i64, DbError> {
    use crate::schema::channel_playback_speed::dsl::*;

    channel_playback_speed
        .filter(account_id.eq(owner_id))
        .count()
        .get_result(conn)
        .await
}

/// Whether storing `incoming` further rows would exceed the per-table quota.
///
/// `incoming` is an upper bound: some of those rows may be updates to existing
/// ones, so this can reject slightly early at the very top of the range.
pub fn exceeds_row_quota(stored_rows: i64, incoming: usize) -> bool {
    let incoming = i64::try_from(incoming).unwrap_or(i64::MAX);

    stored_rows.saturating_add(incoming) > MAX_ROWS_PER_ACCOUNT
}

#[cfg(test)]
mod tests {
    use super::{MAX_ROWS_PER_ACCOUNT, exceeds_row_quota};

    #[test]
    fn quota_allows_up_to_the_limit() {
        assert!(!exceeds_row_quota(0, 1));
        assert!(!exceeds_row_quota(MAX_ROWS_PER_ACCOUNT - 1, 1));
        assert!(exceeds_row_quota(MAX_ROWS_PER_ACCOUNT, 1));
    }

    #[test]
    fn quota_does_not_overflow_on_absurd_batches() {
        assert!(exceeds_row_quota(i64::MAX, usize::MAX));
        assert!(exceeds_row_quota(0, usize::MAX));
    }
}
