use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssVideo {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) published: DateTime<Utc>,
    #[serde(rename = "group")]
    pub(crate) media: RssVideoMedia,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RssVideoMedia {
    pub(crate) thumbnail: String,
}

impl RssVideo {
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn date(&self) -> DateTime<Utc> {
        self.published
    }

    pub fn thumbnail_url(&self) -> &str {
        &self.media.thumbnail
    }
}
