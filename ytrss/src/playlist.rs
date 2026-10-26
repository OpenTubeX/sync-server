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

        Self::from_xml(&response_body)
    }

    pub fn from_xml(xml: &str) -> YouTubeResult<Self> {
        serde_roxmltree::from_str(xml).map_err(YouTubeError::ParserError)
    }

    pub fn video_count(&self) -> usize {
        self.videos.len()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Channel ID of the playlist's owner.
    pub fn channel_id(&self) -> Option<&str> {
        self.author.channel_id()
    }

    /// Create an `RssChannel` view for the uploader of this playlist
    pub fn to_channel(&self) -> RssChannel {
        RssChannel {
            author: self.author.clone(),
            videos: self.videos.clone(),
        }
    }
}
