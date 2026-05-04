//! Validates user-provided data to be valid (to some extent, as it only has limited info due to using YouTube's RSS feeds)

use std::{collections::HashSet, str::FromStr};

use actix_web::{error, http::Uri};

use crate::{
    CONFIG, DbConnection,
    database::{
        channel::get_channel_by_id, public_playlist::get_public_playlist_by_id,
        video::get_video_by_id,
    },
    dto::{CreateVideo, ExtendedPlaylist, ExtendedPublicPlaylist},
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
    // TODO: don't rely on Actix for this, bad separation of concerns
    let Ok(uri) = Uri::from_str(image_url) else {
        return false;
    };

    let Some(host) = uri.host() else {
        return false;
    };

    for thumbnail_domain in ALLOWED_THUMBNAIL_DOMAINS {
        if host.ends_with(thumbnail_domain) {
            return true;
        }
    }

    false
}

fn cmp_title(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
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

pub async fn validate_channel_information_if_changed(
    conn: &mut DbConnection,
    channel: &Channel,
) -> actix_web::Result<()> {
    if !is_channel_validation_required(conn, channel).await {
        return Ok(());
    }

    let rss_channel = RssChannel::fetch_from_channel_id(&channel.id)
        .await
        .map_err(error::ErrorInternalServerError)?;

    validate_channel_information(channel, &rss_channel).map_err(error::ErrorBadRequest)
}

/// Validate if the provided channel information is valid.
/// If yes, the method returns an `Ok` result. If not, the method returns an `Err`
fn validate_channel_information(channel: &Channel, rss_channel: &RssChannel) -> Result<(), String> {
    if !verify_image_url(&channel.avatar) {
        return Err("invalid channel avatar provided".to_string());
    }

    if !cmp_title(rss_channel.name(), &channel.name) {
        return Err("invalid channel information provided".to_string());
    }

    Ok(())
}

/// Requirement: all videos must be from the same channel!
pub async fn validate_video_information_if_changed_single(
    conn: &mut DbConnection,
    video_data: &mut CreateVideo,
) -> actix_web::Result<()> {
    let mut video_datas = vec![video_data.clone()];
    validate_video_information_if_changed(conn, &mut video_datas).await?;
    (*video_data) = video_datas[0].clone();

    Ok(())
}

/// Requirement: all videos must be from the same channel!
pub async fn validate_video_information_if_changed(
    conn: &mut DbConnection,
    video_datas: &mut [CreateVideo],
) -> actix_web::Result<()> {
    if !CONFIG.validate_submitted_metadata {
        return Ok(());
    }

    let channel = video_datas
        .iter()
        .map(|video_data| video_data.uploader.clone())
        .collect::<HashSet<Channel>>();
    if channel.len() != 1 {
        return Err(error::ErrorInternalServerError(
            "can only process videos from the same channel",
        ));
    }
    let channel = channel.iter().next().unwrap();

    let channel_rss = RssChannel::fetch_from_channel_id(&channel.id)
        .await
        .map_err(error::ErrorInternalServerError)?;
    validate_channel_information(channel, &channel_rss).map_err(error::ErrorBadRequest)?;

    for video_data in video_datas.iter_mut() {
        // verification is only required if the channel doesn't exist yet or has changed since then
        let existing_video = get_video_by_id(conn, &video_data.id).await.ok().flatten();

        if let Some(existing_video) = existing_video
            && std::convert::Into::<Video>::into(&*video_data) == existing_video
        {
            continue;
        }

        (*video_data) = validate_video_information(video_data.clone(), &channel_rss)
            .map_err(error::ErrorBadRequest)?;
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
) -> actix_web::Result<ExtendedPublicPlaylist> {
    if !CONFIG.validate_submitted_metadata {
        return Ok(playlist);
    }

    let rss_playlist = RssPlaylist::fetch_from_playlist_id(&playlist.playlist.id)
        .await
        .map_err(error::ErrorInternalServerError)?;

    if is_channel_validation_required(conn, &playlist.uploader).await {
        validate_channel_information(&playlist.uploader, &rss_playlist.to_channel())
            .map_err(error::ErrorBadRequest)?;
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
        .map_err(error::ErrorBadRequest)?;

    Ok(ExtendedPublicPlaylist {
        playlist: validated_playlist,
        uploader: playlist.uploader,
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
    }

    #[actix_rt::test]
    async fn test_channel_validator() {
        let channel_rss = RssChannel::fetch_from_channel_id("UC8-Th83bH_thdKZDJCrn88g")
            .await
            .unwrap();

        assert!(
            validate_channel_information(
                &Channel {
                    id: "UC8-Th83bH_thdKZDJCrn88g".to_string(),
                    name: "The Tonight Show Starring Jimmy Fallon".to_string(),
                    avatar: "https://i1.ytimg.com/vi/hTC6Xa5TrRc/hqdefault.jpg".to_string(),
                    verified: true,
                },
                &channel_rss
            )
            .is_ok()
        );

        assert!(
            validate_channel_information(
                &Channel {
                    id: "UC8-Th83bH_thdKZDJCrn88g".to_string(),
                    name: "The Tonight Show Starring Jimmy Fallon".to_string(),
                    avatar: "https://i1.example.com/vi/hTC6Xa5TrRc/hqdefault.jpg".to_string(),
                    verified: true,
                },
                &channel_rss
            )
            .is_err()
        );

        assert!(
            validate_channel_information(
                &Channel {
                    id: "UC8-Th83bH_thdKZDJCrn88g".to_string(),
                    name: "Wrong channel name".to_string(),
                    avatar: "https://i1.example.com/vi/hTC6Xa5TrRc/hqdefault.jpg".to_string(),
                    verified: true,
                },
                &channel_rss
            )
            .is_err()
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
                avatar: "https://yt3.googleusercontent.com/ytc/AIdro_lBXTw2HqumabqUMrMcWlB5BVUa-bDCP1YQ0Jwf89C6RMY=s160-c-k-c0x00ffffff-no-rj".to_string(),
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
