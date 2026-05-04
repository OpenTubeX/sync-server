mod channel;
mod playlist;
mod video;

pub use channel::RssChannel;
pub use playlist::RssPlaylist;

const YOUTUBE_BASE_URL: &str = "https://www.youtube.com";

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum YouTubeError {
    ConnectionError,
    ParserError(serde_roxmltree::Error),
}

impl std::fmt::Display for YouTubeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            YouTubeError::ConnectionError => write!(f, "failed to connect to youtube"),
            YouTubeError::ParserError(reason) => write!(f, "failed to parse: {reason}"),
        }
    }
}
impl std::error::Error for YouTubeError {}

type YouTubeResult<T> = Result<T, YouTubeError>;
