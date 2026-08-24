//! Validates user-provided data to be valid (to some extent, as it only has limited info due to using YouTube's RSS feeds)

use std::cmp::max;
use std::collections::HashSet;

use itertools::Itertools;

use crate::{
    CONFIG, DbConnection,
    database::{
        channel::get_channel_by_id, public_playlist::get_public_playlist_by_id,
        video::get_video_by_id,
    },
    dto::{CreateVideo, ExtendedPlaylist, ExtendedPublicPlaylist},
    handlers::{HandlerError, HandlerResult},
    models::{Channel, Video},
};

use ytrss::{RssChannel, RssPlaylist};

const ALLOWED_THUMBNAIL_DOMAINS: [&str; 5] = [
    "youtube.com",
    "googlevideo.com",
    "ytimg.com",
    "ggpht.com",
    "googleusercontent.com",
];

fn verify_image_url(image_url: &str) -> bool {
    if image_url.is_empty() {
        return true;
    }

    let Ok(url) = url::Url::parse(image_url) else {
        return false;
    };

    // Clients load these URLs, so do not store plaintext or exotic schemes.
    if url.scheme() != "https" {
        return false;
    }

    let Some(host) = url.host_str() else {
        return false;
    };

    // Match on label boundaries. A plain `ends_with` would also accept
    // attacker-registrable domains such as `evil-youtube.com`.
    ALLOWED_THUMBNAIL_DOMAINS.iter().any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn alphanumeric_words(s: &str) -> Vec<String> {
    s.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .split(" ")
        .map(|w| w.trim().to_string())
        .collect_vec()
}

/// Checks whether the titles look somewhat similar.
fn approx_cmp_title(a: &str, b: &str) -> bool {
    let a_words = alphanumeric_words(a);
    let b_words = alphanumeric_words(b);

    let max_word_count = max(a_words.len(), b_words.len());
    let mut matching_words = 0;
    for a_word in &a_words {
        for b_word in &b_words {
            if a_word.contains(b_word) || b_word.contains(a_word) {
                matching_words += 1;
                break;
            }
        }
    }

    matching_words >= 1 && matching_words > max_word_count / 3
}

async fn is_channel_validation_required(conn: &mut DbConnection, channel: &Channel) -> bool {
    if !CONFIG.validate_submitted_metadata {
        return false;
    }

    // verification is only required if the channel doesn't exist yet or has changed since then
    if let Some(existing_channel) = get_channel_by_id(conn, &channel.id).await.ok().flatten()
        && *channel == existing_channel
    {
        return false;
    }

    true
}

/// Decide which of `channels` still need to be checked against YouTube.
///
/// This is the only phase that needs a database connection. Callers should run
/// it, release their connection, and only then call
/// [`validate_channel_against_youtube`], so that a batch of slow network
/// round-trips never occupies a pooled connection.
///
/// Returned indices are deduplicated by the whole channel value.
///
/// Deduplicating by id alone would be a validation bypass: two entries can share
/// an id but carry different names or avatars, and the caller persists every
/// entry. Skipping the second one would let it reach the database unvalidated.
/// Channels are shared between accounts, so that would also let one account
/// poison a channel's avatar for everyone.
pub async fn channels_requiring_validation(
    conn: &mut DbConnection,
    channels: &[Channel],
) -> Vec<usize> {
    let mut required = Vec::new();

    for index in distinct_channel_indices(channels) {
        if is_channel_validation_required(conn, &channels[index]).await {
            required.push(index);
        }
    }

    required
}

/// Indices of the first occurrence of each distinct channel value.
///
/// Entries that are fully equal are interchangeable, so validating one covers
/// the rest. Entries that differ in any field are kept separately.
fn distinct_channel_indices(channels: &[Channel]) -> Vec<usize> {
    let mut seen = HashSet::new();

    channels
        .iter()
        .enumerate()
        .filter(|(_, channel)| seen.insert(*channel))
        .map(|(index, _)| index)
        .collect()
}

/// Validate a channel against its YouTube RSS feed.
///
/// Deliberately takes no database connection so that callers cannot hold one
/// across the network round-trip.
pub async fn validate_channel_against_youtube(channel: &mut Channel) -> HandlerResult<()> {
    let rss_channel = RssChannel::fetch_from_channel_id(&channel.id)
        .await
        .map_err(|_| HandlerError::YouTubeConnectError)?;

    // Assign the result back: `validate_channel_information` replaces the name
    // with the canonical one from the feed, and dropping it here would persist
    // the client-supplied name instead. `validate_videos_against_youtube` does
    // the same, so both paths normalize identically.
    (*channel) = validate_channel_information(channel.clone(), &rss_channel)
        .map_err(|_| HandlerError::ValidationError)?;

    Ok(())
}

/// Validate if the provided channel information is valid.
/// If yes, the method returns an `Ok` result. If not, the method returns an `Err`
fn validate_channel_information(
    mut channel: Channel,
    rss_channel: &RssChannel,
) -> Result<Channel, String> {
    if let Some(ref avatar) = channel.avatar
        && !verify_image_url(avatar)
    {
        return Err("invalid channel avatar provided".to_string());
    }

    if !approx_cmp_title(rss_channel.name(), &channel.name) {
        return Err("invalid channel information provided".to_string());
    }

    channel.name = rss_channel.name().to_string();
    Ok(channel)
}

/// Mark which videos still differ from the copy already stored, and therefore
/// need to be checked against YouTube.
///
/// This is the only phase that needs a database connection. Run it, release the
/// connection, then call [`validate_videos_against_youtube`].
pub async fn videos_requiring_validation(
    conn: &mut DbConnection,
    video_datas: &[CreateVideo],
) -> Vec<bool> {
    if !CONFIG.validate_submitted_metadata {
        return vec![false; video_datas.len()];
    }

    let mut required = Vec::with_capacity(video_datas.len());
    for video_data in video_datas {
        // verification is only required if the video doesn't exist yet or has changed since then
        let existing_video = get_video_by_id(conn, &video_data.id).await.ok().flatten();
        required.push(
            !existing_video
                .is_some_and(|existing| std::convert::Into::<Video>::into(video_data) == existing),
        );
    }

    required
}

/// Validate a batch of videos, all from `channel`, against the channel's RSS feed.
///
/// `needs_validation` comes from [`videos_requiring_validation`] and must have
/// the same length as `video_datas`. Deliberately takes no database connection
/// so that callers cannot hold one across the network round-trip.
///
/// Requirement: all videos must be from the same channel!
pub async fn validate_videos_against_youtube(
    video_datas: &mut [CreateVideo],
    needs_validation: &[bool],
    channel: &mut Channel,
) -> HandlerResult<()> {
    if !CONFIG.validate_submitted_metadata {
        return Ok(());
    }

    // `zip` below truncates to the shorter slice, which would silently skip
    // validation for the tail. This gates metadata validation, so enforce the
    // contract rather than documenting it.
    if needs_validation.len() != video_datas.len() {
        return Err(HandlerError::ValidationErrorWithContext(
            "validation plan does not match the batch".to_owned(),
        ));
    }

    for video in video_datas.iter() {
        if video.uploader != *channel {
            return Err(HandlerError::ValidationErrorWithContext(
                "can only process videos from the same channel".to_string(),
            ));
        }
    }

    let channel_rss = RssChannel::fetch_from_channel_id(&channel.id)
        .await
        .map_err(|_| HandlerError::YouTubeConnectError)?;
    (*channel) = validate_channel_information(channel.clone(), &channel_rss)
        .map_err(|_| HandlerError::ValidationError)?;

    for (video_data, needs_validation) in video_datas.iter_mut().zip(needs_validation) {
        if !needs_validation {
            continue;
        }

        (*video_data) = validate_video_information(video_data.clone(), &channel_rss)
            .map_err(|_| HandlerError::ValidationError)?;
    }

    Ok(())
}

/// Validates the video exists and returns updated meta information from the RSS feed.
///
/// You should use the resulting [CreateVideo] for doing any further actions with the video,
/// because its metadata is more accurate.
fn validate_video_information(
    video_data: CreateVideo,
    rss_channel: &RssChannel,
) -> Result<CreateVideo, String> {
    // validate thumbnail URL
    if !verify_image_url(&video_data.thumbnail_url) {
        return Err("invalid channel information provided".to_string());
    }

    let Some(oldest_date) = rss_channel.oldest_video_date() else {
        return Ok(video_data);
    };

    // Video is older than the videos in the feed
    if oldest_date.timestamp_millis() > video_data.upload_date {
        return Ok(video_data);
    }

    let Some(rss_video) = rss_channel.find_video(&video_data.id) else {
        return Ok(video_data);
    };

    let mut video_data = video_data;
    video_data.title = rss_video.title().to_string();
    video_data.upload_date = rss_video.date().timestamp_millis();
    video_data.thumbnail_url = rss_video.thumbnail_url().to_string();

    Ok(video_data)
}

pub async fn validate_public_playlist_information_if_changed(
    conn: &mut DbConnection,
    playlist: ExtendedPublicPlaylist,
) -> HandlerResult<ExtendedPublicPlaylist> {
    if !CONFIG.validate_submitted_metadata {
        return Ok(playlist);
    }

    let rss_playlist = RssPlaylist::fetch_from_playlist_id(&playlist.playlist.id)
        .await
        .map_err(|_| HandlerError::YouTubeConnectError)?;

    let mut uploader = playlist.uploader.clone();
    if is_channel_validation_required(conn, &uploader).await {
        uploader = validate_channel_information(uploader, &rss_playlist.to_channel())
            .map_err(|_| HandlerError::ValidationError)?;
    }

    // verification is only required if the channel doesn't exist yet or has changed since then
    if let Some(existing_playlist) = get_public_playlist_by_id(conn, &playlist.playlist.id)
        .await
        .ok()
        .flatten()
        && playlist.playlist == ExtendedPlaylist::from_public_playlist(&existing_playlist)
    {
        return Ok(playlist);
    }

    let validated_playlist = validate_playlist_information(playlist.playlist, &rss_playlist)
        .map_err(|_| HandlerError::ValidationError)?;

    Ok(ExtendedPublicPlaylist {
        playlist: validated_playlist,
        uploader,
    })
}

// Update the given playlist based on the playlist's RSS feed.
// This can only validate the title as that's the only info available in the channel.
fn validate_playlist_information(
    mut playlist: ExtendedPlaylist,
    feed_rss: &RssPlaylist,
) -> Result<ExtendedPlaylist, String> {
    if let Some(video_count) = playlist.video_count
        && feed_rss.video_count() > video_count as usize
    {
        return Err("video count is less than actual amount of videos".to_string());
    }

    playlist.title = feed_rss.title().to_string();
    Ok(playlist)
}

#[cfg(test)]
mod test {
    use ytrss::{RssChannel, RssPlaylist};

    use crate::{
        dto::{CreateVideo, ExtendedPlaylist},
        models::Channel,
        validation::{
            validate_channel_information, validate_playlist_information,
            validate_video_information, verify_image_url,
        },
    };

    fn channel(id: &str, name: &str) -> Channel {
        Channel {
            id: id.to_owned(),
            name: name.to_owned(),
            avatar: Some("https://i1.ytimg.com/vi/x/hqdefault.jpg".to_owned()),
            verified: false,
        }
    }

    /// Regression test: deduplicating by id alone let a second entry sharing an
    /// id but carrying forged fields reach the database unvalidated.
    #[test]
    fn dedup_keeps_channels_that_share_an_id_but_differ() {
        let channels = [
            channel("UC_same", "Real Name"),
            channel("UC_same", "Forged Name"),
        ];

        assert_eq!(
            crate::validation::distinct_channel_indices(&channels),
            vec![0, 1]
        );
    }

    #[test]
    fn dedup_collapses_fully_equal_channels() {
        let channels = [
            channel("UC_a", "Name"),
            channel("UC_a", "Name"),
            channel("UC_b", "Other"),
        ];

        assert_eq!(
            crate::validation::distinct_channel_indices(&channels),
            vec![0, 2]
        );
    }

    #[test]
    fn test_image_url_validator() {
        assert!(verify_image_url(
            "https://i1.ytimg.com/vi/hTC6Xa5TrRc/hqdefault.jpg"
        ));
        assert!(verify_image_url(
            "https://ytimg.com/vi/hTC6Xa5TrRc/hqdefault.jpg"
        ));
        assert!(!verify_image_url(
            "https://mydomain.com/vi/hTC6Xa5TrRc/hqdefault.jpg"
        ));
        // suffix matches that are not label boundaries must be rejected
        assert!(!verify_image_url("https://evil-youtube.com/a.jpg"));
        assert!(!verify_image_url("https://notytimg.com/a.jpg"));
        assert!(!verify_image_url("https://ytimg.com.evil.net/a.jpg"));
        // real subdomains are still accepted
        assert!(verify_image_url("https://yt3.googleusercontent.com/a.jpg"));
        // only https, since clients load whatever is stored here
        assert!(!verify_image_url("http://i1.ytimg.com/vi/x/hqdefault.jpg"));
        assert!(!verify_image_url("ftp://i1.ytimg.com/vi/x/hqdefault.jpg"));
    }

    #[actix_rt::test]
    async fn test_channel_validator() {
        let channel_rss = RssChannel::fetch_from_channel_id("UC8-Th83bH_thdKZDJCrn88g")
            .await
            .unwrap();

        assert!(
            validate_channel_information(
                Channel {
                    id: "UC8-Th83bH_thdKZDJCrn88g".to_string(),
                    name: "The Tonight Show Starring Jimmy Fallon".to_string(),
                    avatar: Some("https://i1.ytimg.com/vi/hTC6Xa5TrRc/hqdefault.jpg".to_string(),),
                    verified: true,
                },
                &channel_rss
            )
            .is_ok()
        );

        assert!(
            validate_channel_information(
                Channel {
                    id: "UC8-Th83bH_thdKZDJCrn88g".to_string(),
                    name: "The Tonight Show Starring Jimmy Fallon".to_string(),
                    avatar: Some(
                        "https://i1.example.com/vi/hTC6Xa5TrRc/hqdefault.jpg".to_string(),
                    ),
                    verified: true,
                },
                &channel_rss
            )
            .is_err()
        );

        assert!(
            validate_channel_information(
                Channel {
                    id: "UC8-Th83bH_thdKZDJCrn88g".to_string(),
                    name: "Wrong channel name".to_string(),
                    avatar: Some(
                        "https://i1.example.com/vi/hTC6Xa5TrRc/hqdefault.jpg".to_string(),
                    ),
                    verified: true,
                },
                &channel_rss
            )
            .is_err()
        );
    }

    #[actix_rt::test]
    async fn test_channel_validator_mismatching_uploader_names() {
        // the RSS uploader name is different to the one on the web UI
        let channel_rss = RssChannel::fetch_from_channel_id("UCjp_3PEaOau_nT_3vnqKIvg")
            .await
            .unwrap();

        assert!(
            validate_channel_information(
                Channel {
                    id: "UCjp_3PEaOau_nT_3vnqKIvg".to_string(),
                    name: "Junya Official Channel".to_string(),
                    avatar: Some("https://yt3.googleusercontent.com/ytc/AIdro_mFt9iiVlgxD1gBW74I1o6H8xFtOg5AwqPj2_1JKHJ4UJg=s160-c-k-c0x00ffffff-no-rj".to_string()),
                    verified: true,
                },
                &channel_rss
            )
            .is_ok()
        );
    }

    #[actix_rt::test]
    async fn test_video_validator() {
        let video = CreateVideo {
            id: "kMO1L5J1cn8".to_string(),
            title: "Minecraft Livestream [FaceCam] | Kotti".to_string(),
            upload_date: 1549036231000, /* 2019-02-01T16:50:31+00:00 */
            thumbnail_url: "https://i4.ytimg.com/vi/kMO1L5J1cn8/hqdefault.jpg".to_string(),
            duration: 4352,
            uploader: Channel {
                id: "UCWnQYRWgTbsLTDOAVc3uzRg".to_string(),
                name: "KottiXD".to_string(),
                avatar: Some("https://yt3.googleusercontent.com/ytc/AIdro_lBXTw2HqumabqUMrMcWlB5BVUa-bDCP1YQ0Jwf89C6RMY=s160-c-k-c0x00ffffff-no-rj".to_string()),
                verified: false,
            },
        };

        let channel_rss = RssChannel::fetch_from_channel_id(&video.uploader.id)
            .await
            .unwrap();
        assert!(validate_video_information(video, &channel_rss).is_ok());
    }

    #[actix_rt::test]
    async fn test_playlist_validator() {
        let channel_rss = RssPlaylist::fetch_from_playlist_id("PLI-n-55RUT-_Ej39IlAxon_hOJWeET7cI")
            .await
            .unwrap();

        let playlist = ExtendedPlaylist {
            id: "PLI-n-55RUT-_Ej39IlAxon_hOJWeET7cI".to_string(),
            title: "Best German Songs".to_string(),
            description: "Songs 2026 - Songs with Lyrics Playlist - My Mix - Mix Songs - Music Playlist 2026. Welcome to a curated playlist featuring the best English songs with lyrics that speak to the heart. Sing along to powerful lyrics that capture the essence of love, life, and everything in between. Mix, songs 2026, new songs 2026, top songs, best songs, my mix, mix songs, songs mix, my mix playlist, songs playlist, songs with lyrics playlist, my playlist, good songs, english songs. Songs January 2026, february 2026, march 2026, april 2026, may 2026, june 2026, july 2026, august 2026, september 2026, october 2026, november 2026, december 2026 etc. Songs 2027 - music playlist 2025.".to_string(),
            thumbnail_url: Some("https://i.ytimg.com/vi/M1P0HAr-8zg/hqdefault.jpg?sqp=-oaymwEXCNACELwBSFryq4qpAwkIARUAAIhCGAE=&rs=AOn4CLBXQ360CqPdgkFrha1H3l9cx23I8A".to_string()),
            video_count: Some(120),
        };

        assert!(validate_playlist_information(playlist, &channel_rss).is_ok());

        let playlist = ExtendedPlaylist {
            id: "PLI-n-55RUT-_Ej39IlAxon_hOJWeET7cI".to_string(),
            title: "Best German Songs".to_string(),
            description: "".to_string(),
            thumbnail_url: Some("https://i.ytimg.com/vi/M1P0HAr-8zg/hqdefault.jpg?sqp=-oaymwEXCNACELwBSFryq4qpAwkIARUAAIhCGAE=&rs=AOn4CLBXQ360CqPdgkFrha1H3l9cx23I8A".to_string()),
            video_count: Some(0), // impossible video count because feed is larger than 0
        };

        assert!(validate_playlist_information(playlist, &channel_rss).is_err());
    }
}
