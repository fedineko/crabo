use std::collections::HashMap;
use log::{debug, warn};
use serde::Deserialize;
use url::Url;
use crabo_model::Snapshot;
use crate::snapper::{CacheHints, Clients, Snapper, SnapshotAndHints};

/// This snapper uses YouTube official API to get video details.
pub(crate) struct YoutubeSnapper {
    /// API key to access YouTube API v3
    api_key: String,
}

/// Thumbnail image details.
#[derive(Deserialize)]
#[derive(Clone)]
struct Thumbnail {
    /// Actual image data url.
    url: Option<Url>,
}

/// Keeps basic details about video.
#[derive(Deserialize)]
#[derive(Clone)]
struct Snippet {
    /// Video title.
    title: Option<String>,

    /// Description of the video.
    description: Option<String>,

    /// Collection of thumbnails.
    thumbnails: HashMap<String, Thumbnail>,

    /// Video tags.
    tags: Option<Vec<String>>,
}

/// Wrapper Video object.
#[derive(Deserialize)]
#[derive(Clone)]
struct Video {
    /// Video details snippet.
    snippet: Snippet,
}

/// Response expected for meta-data request.
#[derive(Deserialize)]
struct VideoListResponse {
    #[serde(alias = "items")]
    videos: Vec<Video>,
}

/// This function extract video ID from `url` to pass it later in API request.
/// It supports both shorter youtu.be and full size youtube.com formats.
fn extract_video_id(url: &Url) -> Option<String> {
    let host = url.host()?.to_string();

    if host.ends_with("youtu.be") {
        let path = url.path();

        return if path.is_empty() {
            None
        } else {
            Some(path[1..].to_string())
        };
    }

    if host.ends_with("youtube.com") {
        return url.query_pairs()
            .find(|(k, _)| k == "v")
            .map(|(_, v)| v.to_string());
    }


    debug!("Could not extract YouTube video ID from URL {url}");

    None
}

impl YoutubeSnapper {
    /// Constructs new instance of [YoutubeSnapper].
    pub(crate) fn new(api_key: String) -> Self {
        Self {
            api_key
        }
    }

    /// Produces Crabo's [Snapshot] from YouTube's `video` and `thumbnail`,
    /// acquired for the processed `url`.
    fn thumbnail_to_snapshot(
        &self,
        url: Url,
        video: Video,
        thumbnail: Option<Thumbnail>,
    ) -> Option<Snapshot> {
        match thumbnail {
            Some(thumbnail) => {
                let preview_url = thumbnail.url.clone();

                let preview_mime_type = thumbnail.url.map(
                    |x| mime_guess::from_path(x.path())
                )
                    .and_then(|m| m.first())
                    .map(|m| m.to_string());

                Some(Snapshot {
                    url,
                    preview_url,
                    title: video.snippet.title,
                    description: video.snippet.description,
                    source: Option::from("YouTube".to_string()),

                    tags: video.snippet.tags.into_iter()
                        .flatten()
                        .map(|tag| format!("#{tag}"))
                        .collect(),

                    preview_mime_type,
                    application_name: None,
                })
            }
            None => None,
        }
    }
}

impl Snapper for YoutubeSnapper {
    fn cache_hints(&self, video_url: &Url) -> Option<CacheHints> {
        extract_video_id(video_url)
            .map(|id| CacheHints {
                provider: "youtube".into(),
                id,
            })
    }

    async fn snap(
        &self,
        url: Url,
        cache_hints: CacheHints,
        clients: &Clients
    ) -> SnapshotAndHints {
        let video_id = &cache_hints.id;
        let api_key = &self.api_key;

        let query_url_str = format!(
            "https://www.googleapis.com/youtube/v3/videos?\
            id={video_id}&\
            key={api_key}&\
            part=snippet&\
            fields=items(id,snippet)"
        );

        let query_url = Url::parse(&query_url_str).unwrap();

        match clients.generic_client.get_json::<VideoListResponse>(
            &query_url,
            None
        ).await {
            Ok(response) => {
                let snapshot = response.videos.into_iter()
                    .next()
                    .and_then(|video| {
                        // Types of thumbnail according to
                        // https://developers.google.com/youtube/v3/docs/videos#snippet.thumbnails
                        // -----------------------------------------------------------------------
                        //   default  –  120px x 90px
                        //   medium   –  320px x 180px
                        //   high     –  480px x 360px
                        //   standard –  640px x 480px (available for some videos)
                        //   maxres   – 1280px x 720px (available for some videos).

                        let thumbnail = [
                            "high",
                            "standard",
                            "maxres",
                            "medium",
                            "default"
                        ].into_iter()
                            .filter_map(
                                |key| video.snippet.thumbnails.get(key)
                            )
                            .next();

                        self.thumbnail_to_snapshot(
                            url,
                            video.clone(),
                            thumbnail.cloned()
                        )
                    });

                SnapshotAndHints {
                    snapshot,
                    hints: cache_hints,
                }
            }

            Err(err) => {
                warn!(
                    "Failed to get details for YouTube '{video_id}', \
                    API call result is: {err:?}"
                );

                SnapshotAndHints {
                    snapshot: None,
                    hints: cache_hints,
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use url::Url;
    use crate::youtube::extract_video_id;

    #[test]
    fn test_youtu_be() {
        let url = Url::parse("https://youtu.be/x8?si=HxxxJ").unwrap();
        assert_eq!(extract_video_id(&url), Some("x8".to_string()));
    }
}