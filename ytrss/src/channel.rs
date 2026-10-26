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
    #[serde(default)]
    pub(crate) uri: Option<String>,
}

impl RssChannelAuthor {
    /// The author's channel ID, taken from their `/channel/<id>` URI.
    pub(crate) fn channel_id(&self) -> Option<&str> {
        self.uri
            .as_deref()?
            .rsplit_once("/channel/")
            .map(|(_, id)| id)
            .filter(|id| !id.is_empty())
    }
}

impl RssChannel {
    pub async fn fetch_from_channel_id(channel_id: &str) -> YouTubeResult<Self> {
        let response_body = crate::fetch_feed("channel_id", channel_id).await?;

        Self::from_xml(&response_body)
    }

    pub fn from_xml(xml: &str) -> YouTubeResult<Self> {
        serde_roxmltree::from_str(xml).map_err(YouTubeError::ParserError)
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
