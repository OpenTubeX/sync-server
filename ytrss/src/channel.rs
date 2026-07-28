use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::{YouTubeError, YouTubeResult, video::RssVideo};

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssChannel {
    pub(crate) author: RssChannelAuthor,
    #[serde(rename = "entry")]
    pub(crate) videos: Vec<RssVideo>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub(crate) struct RssChannelAuthor {
    pub(crate) name: String,
}

impl RssChannel {
    pub async fn fetch_from_channel_id(channel_id: &str) -> YouTubeResult<Self> {
        let response_body = crate::fetch_feed("channel_id", channel_id).await?;

        serde_roxmltree::from_str(&response_body).map_err(YouTubeError::ParserError)
    }

    pub fn name(&self) -> &str {
        &self.author.name
    }

    pub fn oldest_video_date(&self) -> Option<DateTime<Utc>> {
        self.videos.last().map(|vid| vid.published)
    }

    pub fn find_video(&self, id: &str) -> Option<&RssVideo> {
        self.videos.iter().find(|vid| vid.id == id)
    }
}
