use actix_web::dev::ResourcePath;
use log::{debug, warn};
use serde::Deserialize;

use crabo_model::Snapshot;
use fedineko_http_client::GenericClient;

use crate::snapper::{CacheHints, Clients, Snapper, SnapshotAndHints};

/// This is barebones implementation of API to get video information from
/// BiliBili.
///
/// API endpoint was taken from <https://github.com/Nemo2011/bilibili-api>
pub(crate) struct BiliBiliSnapper {}

/// A very simplified version of BiliBili's video data.
#[derive(Deserialize)]
#[derive(Clone)]
struct VideoData {
    /// Thumbnail image reference.
    pic: Option<url::Url>,

    /// Video title.
    title: Option<String>,

    /// Video description.
    desc: Option<String>,
}

/// Example response
/// ```{
///   "code": 0,
///   "message": "0",
///   "data": {
///     "bvid": "BVxxxxxxxx",
///     "aid": 3xxxxxxxxx,
///     "pic": "https://domain/path/image",
///     "title": "...",
///     "pubdate": 1234567890,
///     "desc": "...",
///    ...
///    }
/// ```
#[derive(Deserialize)]
struct BiliBiliResponse {
    data: VideoData,
}

/// This function extracts BiliBili video ID from `url`,
/// and returns either that ID or None.
fn extract_video_id(url: &url::Url) -> Option<String> {
    let host = url.host()?.to_string();

    if host.ends_with("b23.tv") {
        // that's short URL that needs to be resolved
        return Some(host.path().to_string());
    }

    if !host.ends_with("bilibili.com") {
        debug!("Could not extract BiliBili video ID from URL {}", url);
        return None;
    }

    url.path()
        .strip_prefix("/video/")
        .map(|s| s.trim_matches('/').to_string())
}

impl BiliBiliSnapper {
    /// This method converts `video` data to Crabo [Snapshot].
    /// On success returns instance of [Snapshot], otherwise None is returned.
    fn videodata_to_snapshot(
        &self,
        url: url::Url,
        video: VideoData,
    ) -> Option<Snapshot> {
        let preview_mime_type = video.pic.as_ref()
            .map(|x| mime_guess::from_path(x.path()))
            .and_then(|m| m.first())
            .map(|m| m.to_string());

        Some(Snapshot {
            url,
            preview_url: video.pic,
            title: video.title,
            description: video.desc,
            source: Option::from("BiliBili".to_string()),
            tags: Vec::default(),
            preview_mime_type,
            application_name: None,
        })
    }

    /// This method attempts to resolve shortened URL represented by `id`
    /// to actual video ID. `client` is used to make requests.
    /// Returns either resolved video ID or None.
    async fn resolve_short_url(
        id: &str,
        client: &GenericClient,
    ) -> Option<String> {
        let url = url::Url::parse("https://b23.tv")
            .and_then(|u| u.join(id))
            .unwrap();

        let headers = match client.head(&url).await {
            Ok(headers) => headers,

            Err(err) => {
                warn!("Failed to resolve short URL {url}: {err:?}");
                return None;
            }
        };

        headers.get("location")
            .map(|value| url::Url::parse(value.to_str().unwrap()).unwrap())
            .and_then(|url| extract_video_id(&url))
    }
}

impl Snapper for BiliBiliSnapper {
    fn cache_hints(&self, video_url: &url::Url) -> Option<CacheHints> {
        extract_video_id(video_url)
            .map(|id| CacheHints {
                provider: "bilibili".into(),
                id,
            })
    }

    async fn snap(
        &self,
        url: url::Url,
        cache_hints: CacheHints,
        clients: &Clients,
    ) -> SnapshotAndHints {
        // if ID in hints does not look like video ID, then earlier that ID
        // was extracted from short URL and needs to be resolved to a proper
        // video address from which video ID will be extracted, so in turn it
        // could be fed to API endpoint.
        // ...
        // Yes, it is a very unnatural way of doing things.
        //
        // Maybe it is better to resolve in cache_hints() instead and revamp
        // synchronous code there.
        let video_id = if !cache_hints.id.starts_with("BV") {
            Self::resolve_short_url(&cache_hints.id, &clients.no_follow_client)
                .await
                .unwrap_or(cache_hints.id.clone())
        } else {
            cache_hints.id.clone()
        };

        // TODO: this URL construction is flawed,
        //       needs a proper URL parameters join.
        let query_url_str = format!(
            "https://api.bilibili.com/x/web-interface/view?bvid={video_id}"
        );

        let query_url = url::Url::parse(&query_url_str).unwrap();

        match clients.generic_client.get_json::<BiliBiliResponse>(
            &query_url,
            None,
        ).await {
            Ok(response) => {
                let snapshot = self.videodata_to_snapshot(url, response.data);

                SnapshotAndHints {
                    snapshot,
                    hints: cache_hints,
                }
            }

            Err(err) => {
                warn!(
                    "Failed to get details for BiliBili video '{video_id}', \
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

    use crate::bilibili::extract_video_id;

    #[test]
    fn test_bilibili_video_id_extraction() {
        let url = Url::parse(
            "https://www.bilibili.com/video/BV1a2b3c/\
            ?share_source=copy_web&vd_source=abcxyz"
        ).unwrap();

        assert_eq!(extract_video_id(&url), Some("BV1a2b3c".to_string()));
    }
}