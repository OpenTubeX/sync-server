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
/// Callers persisting a batch should pass it through [`dedup_channels`] first.
pub async fn channels_requiring_validation(
    conn: &mut DbConnection,
    channels: &[Channel],
) -> Vec<usize> {
    let mut required = Vec::new();

    for (index, channel) in channels.iter().enumerate() {
        if is_channel_validation_required(conn, channel).await {
            required.push(index);
        }
    }

    required
}

/// Drop later copies of fully equal channels, keeping the first occurrence.
///
/// Validation rewrites each entry in place, so a duplicate that is only skipped
/// would still be persisted with its submitted name and overwrite the canonical
/// one. Channels are shared between accounts, so the batch must contain each
/// value once.
///
/// Deduplicating by id alone would be a validation bypass instead: two entries
/// can share an id but carry different names or avatars, so entries that differ
/// in any field are kept and validated separately.
pub fn dedup_channels(channels: &mut Vec<Channel>) {
    let mut seen = HashSet::new();
    channels.retain(|channel| seen.insert(channel.clone()));
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
/// Returns, per video, whether its metadata may replace a stored copy. That is
/// only true for videos the feed confirmed. Other metadata is client-supplied,
/// and videos are shared between accounts, so callers must only insert those
/// when no copy exists yet.
///
/// Requirement: all videos must be from the same channel!
pub async fn validate_videos_against_youtube(
    video_datas: &mut [CreateVideo],
    needs_validation: &[bool],
    channel: &mut Channel,
) -> HandlerResult<Vec<bool>> {
    if !CONFIG.validate_submitted_metadata {
        return Ok(vec![true; video_datas.len()]);
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
    validate_videos_against_feed(video_datas, needs_validation, channel, &channel_rss)
}

/// The part of [`validate_videos_against_youtube`] that runs once the feed is loaded.
fn validate_videos_against_feed(
    video_datas: &mut [CreateVideo],
    needs_validation: &[bool],
    channel: &mut Channel,
    channel_rss: &RssChannel,
) -> HandlerResult<Vec<bool>> {
    (*channel) = validate_channel_information(channel.clone(), channel_rss)
        .map_err(|_| HandlerError::ValidationError)?;

    let mut verified = Vec::with_capacity(video_datas.len());
    for (video_data, needs_validation) in video_datas.iter_mut().zip(needs_validation) {
        // Callers persist each video's own uploader, so it must carry the
        // canonical channel rather than the submitted one.
        video_data.uploader = channel.clone();

        // Matching the stored copy when it was read doesn't make it verified:
        // another entry or request may have corrected that copy since.
        if !needs_validation {
            verified.push(false);
            continue;
        }

        match validate_video_information(video_data.clone(), channel_rss)
            .map_err(|_| HandlerError::ValidationError)?
        {
            Some(validated) => {
                (*video_data) = validated;
                verified.push(true);
            }
            None => verified.push(false),
        }
    }

    Ok(verified)
}

/// Returns the video with its metadata taken from the RSS feed, or [None] if
/// the feed doesn't contain it and the metadata therefore can't be verified.
///
/// You should use the resulting [CreateVideo] for doing any further actions with the video,
/// because its metadata is more accurate.
fn validate_video_information(
    video_data: CreateVideo,
    rss_channel: &RssChannel,
) -> Result<Option<CreateVideo>, String> {
    // validate thumbnail URL
    if !verify_image_url(&video_data.thumbnail_url) {
        return Err("invalid channel information provided".to_string());
    }

    // The feed only lists a channel's latest uploads, so older videos are
    // legitimately missing from it.
    let Some(rss_video) = rss_channel.find_video(&video_data.id) else {
        return Ok(None);
    };

    let mut video_data = video_data;
    video_data.title = rss_video.title().to_string();
    video_data.upload_date = rss_video.date().timestamp_millis();
    video_data.thumbnail_url = rss_video.thumbnail_url().to_string();

    Ok(Some(video_data))
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

    validate_playlist_owner(&playlist.uploader, &rss_playlist)?;

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
        return Ok(ExtendedPublicPlaylist {
            playlist: playlist.playlist,
            uploader,
        });
    }

    let validated_playlist = validate_playlist_information(playlist.playlist, &rss_playlist)
        .map_err(|_| HandlerError::ValidationError)?;

    Ok(ExtendedPublicPlaylist {
        playlist: validated_playlist,
        uploader,
    })
}

/// Bind the submitted uploader to the playlist's owner before trusting its name.
///
/// The name is the only author detail compared otherwise, so without this any
/// playlist would vouch for metadata of an arbitrary, shared channel.
fn validate_playlist_owner(uploader: &Channel, rss_playlist: &RssPlaylist) -> HandlerResult<()> {
    if rss_playlist.channel_id() != Some(uploader.id.as_str()) {
        return Err(HandlerError::ValidationErrorWithContext(
            "uploader doesn't own the playlist".to_string(),
        ));
    }

    Ok(())
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
            validate_channel_information, validate_playlist_information, validate_playlist_owner,
            validate_video_information, validate_videos_against_feed, verify_image_url,
        },
    };

    /// A feed as served by YouTube, with one video entry.
    fn feed(kind: &str, owner_id: &str, owner_name: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
 <id>yt:{kind}</id>
 <title>{owner_name}</title>
 <author>
  <name>{owner_name}</name>
  <uri>https://www.youtube.com/channel/{owner_id}</uri>
 </author>
 <entry>
  <id>yt:video:in_feed</id>
  <yt:videoId>in_feed</yt:videoId>
  <yt:channelId>{owner_id}</yt:channelId>
  <title>Real Title</title>
  <published>2026-10-01T00:00:00+00:00</published>
  <media:group>
   <media:title>Real Title</media:title>
   <media:thumbnail url="https://i1.ytimg.com/vi/in_feed/hqdefault.jpg" width="480" height="360"/>
  </media:group>
 </entry>
</feed>"#
        )
    }

    fn channel_feed() -> RssChannel {
        RssChannel::from_xml(&feed("channel", "UC_channel", "Canonical Name")).unwrap()
    }

    fn video(id: &str, uploader: Channel) -> CreateVideo {
        CreateVideo {
            id: id.to_owned(),
            title: "Forged Title".to_owned(),
            // older than everything in the feed
            upload_date: 0,
            thumbnail_url: "https://i1.ytimg.com/vi/forged/hqdefault.jpg".to_owned(),
            duration: 60,
            uploader,
        }
    }

    /// Regression test: a submitted upload date older than the feed skipped the
    /// lookup, so forged metadata for a video in the feed was accepted.
    #[test]
    fn videos_in_the_feed_take_its_metadata() {
        let validated = validate_video_information(
            video("in_feed", channel("UC_channel", "Canonical")),
            &channel_feed(),
        )
        .unwrap()
        .expect("the video is in the feed");

        assert_eq!(validated.title, "Real Title");
        assert_eq!(
            validated.thumbnail_url,
            "https://i1.ytimg.com/vi/in_feed/hqdefault.jpg"
        );
        assert_eq!(validated.upload_date, 1_790_812_800_000);
    }

    /// Regression test: videos missing from the feed were reported as valid, so
    /// their unverified metadata replaced the shared copy.
    #[test]
    fn videos_missing_from_the_feed_are_unverified() {
        let unverified = video("elsewhere", channel("UC_channel", "Canonical"));

        assert!(
            validate_video_information(unverified, &channel_feed())
                .unwrap()
                .is_none()
        );
    }

    /// Regression test: only the grouped channel was normalized, while callers
    /// persisted each video's uploader with the submitted name.
    #[test]
    fn validated_videos_carry_the_canonical_uploader() {
        let mut uploader = channel("UC_channel", "canonical");
        let mut videos = [
            video("in_feed", uploader.clone()),
            video("elsewhere", uploader.clone()),
            video("stored", uploader.clone()),
        ];

        let verified = validate_videos_against_feed(
            &mut videos,
            &[true, true, false],
            &mut uploader,
            &channel_feed(),
        )
        .unwrap();

        assert_eq!(uploader.name, "Canonical Name");
        assert_eq!(verified, [true, false, false]);
        for video in &videos {
            assert_eq!(video.uploader, uploader);
        }
    }

    /// Regression test: a playlist's author name vouched for any submitted
    /// channel ID.
    #[test]
    fn playlist_uploader_must_own_the_playlist() {
        let playlist = RssPlaylist::from_xml(&feed("playlist", "UC_owner", "Owner")).unwrap();

        assert!(validate_playlist_owner(&channel("UC_owner", "Owner"), &playlist).is_ok());
        assert!(validate_playlist_owner(&channel("UC_victim", "Owner"), &playlist).is_err());
    }

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
        let mut channels = vec![
            channel("UC_same", "Real Name"),
            channel("UC_same", "Forged Name"),
        ];
        let expected = channels.clone();

        crate::validation::dedup_channels(&mut channels);
        assert_eq!(channels, expected);
    }

    /// Regression test: only the first of two equal entries was normalized, and
    /// the second one then overwrote the canonical name with the submitted one.
    #[test]
    fn dedup_collapses_fully_equal_channels() {
        let mut channels = vec![
            channel("UC_a", "Name"),
            channel("UC_a", "Name"),
            channel("UC_b", "Other"),
        ];

        crate::validation::dedup_channels(&mut channels);
        assert_eq!(
            channels,
            vec![channel("UC_a", "Name"), channel("UC_b", "Other")]
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
