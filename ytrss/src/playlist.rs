use serde::Deserialize;

use crate::{RssChannel, YouTubeError, YouTubeResult, channel::RssChannelAuthor, video::RssVideo};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssPlaylist {
    author: RssChannelAuthor,
    title: String,
    #[serde(rename = "entry")]
    videos: Vec<RssVideo>,
}

impl RssPlaylist {
    pub async fn fetch_from_playlist_id(playlist_id: &str) -> YouTubeResult<Self> {
        let response_body = crate::fetch_feed("playlist_id", playlist_id).await?;

        serde_roxmltree::from_str(&response_body).map_err(YouTubeError::ParserError)
    }

    pub fn video_count(&self) -> usize {
        self.videos.len()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Create an `RssChannel` view for the uploader of this playlist
    pub fn to_channel(&self) -> RssChannel {
        RssChannel {
            author: self.author.clone(),
            videos: self.videos.clone(),
        }
    }
}
