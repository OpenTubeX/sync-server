use std::time::Duration;

mod channel;
mod playlist;
mod video;

pub use channel::RssChannel;
pub use playlist::RssPlaylist;

const YOUTUBE_BASE_URL: &str = "https://www.youtube.com";

/// Upper bound for a single feed fetch. Without this a stalled connection blocks
/// the calling request, and anything it holds, indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

// One client per thread rather than one global client. A `reqwest::Client`
// caches idle connections whose IO resources belong to the runtime that created
// them, and actix-web runs a separate current-thread runtime per worker, so a
// shared client would fail once a second worker used it.
thread_local! {
    static CLIENT: reqwest::Client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .expect("failed to build the YouTube HTTP client");
}

/// Cloning shares the calling thread's connection pool, which is cheap and keeps
/// the returned client tied to the current runtime.
fn client() -> reqwest::Client {
    CLIENT.with(reqwest::Client::clone)
}

/// Fetch a YouTube RSS feed selected by a single query parameter.
///
/// The value is appended through `query_pairs_mut` so that it is percent-encoded
/// and cannot inject further query parameters into the request.
pub(crate) async fn fetch_feed(parameter: &str, value: &str) -> YouTubeResult<String> {
    let mut url = reqwest::Url::parse(&format!("{YOUTUBE_BASE_URL}/feeds/videos.xml"))
        .map_err(|_err| YouTubeError::ConnectionError)?;
    url.query_pairs_mut().append_pair(parameter, value);

    client()
        .get(url)
        .send()
        .await
        .map_err(|_err| YouTubeError::ConnectionError)?
        .text()
        .await
        .map_err(|_err| YouTubeError::ConnectionError)
}

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
