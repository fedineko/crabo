use std::collections::HashMap;
use log::{info, warn};
use lol_html::{element, HtmlRewriter, Settings, text};
use tokio_util::bytes;
use url::{ParseError, Url};
use crabo_model::Snapshot;
use bytes::Bytes;
use itertools::Itertools;
use fedineko_http_client::{ClientError, GenericClient};
use crate::robots::RobotsValidator;
use crate::snapper::{CacheHints, Clients, Snapper, SnapshotAndHints};
use crate::util::guess_mime_from_url;

/// If this key is set to "true" then Crabo can make snapshots of page.
///
/// Crabo can try to produce snapshot for mention or RT link,
/// which is undesired if points to e.g. social networking site.
///
/// Most ActivityPub instances opt-out from indexing and Crabo follows
/// "robots" meta-tags like:
/// ```html
///  <meta name="robots" content="noindex">
///  <meta name="fedineko-crabo" content="noindex">
///  <meta name="fedineko-crabo, some-other-bot" content="noindex, noarchive">
/// ```
/// by basic substring match. Crabo also follows robots.txt instructions.
///
/// This affects Crabo only as it makes snippets of web-pages with accepted
/// content type specified as text/html. Other Fedineko components work with
/// ActivityPub and get instructions from related attributes of content or
/// actor's account.
const FEDINEKO_CAN_INDEX_KEY: &str = "fedineko-can-index";

/// Snapper that extracts OpenGraph and similar meta-data from HTML page.
pub(crate) struct HtmlMetaSnapper {
    robots_validator: RobotsValidator,
}

impl HtmlMetaSnapper {
    /// This method constructs new instance of [HtmlMetaSnapper] with default
    /// robots.txt validator settings. Crabo uses 'fedineko-crabo' to
    /// identify itself when parsing robots.txt or robots meta tag.
    pub fn new() -> Self {
        Self {
            robots_validator: RobotsValidator::new("fedineko-crabo")
        }
    }
}

/// A tiny helper function that returns true if `text` contains known
/// instruction to deny index.
fn cannot_index(text: &str) -> bool {
    text.contains("noindex") |
        text.contains("none") |
        text.contains("nosnippet")
}

/// This function parses HTML `bytes` using [lol_html] streaming parser.
///
/// Returns map of properties extracted from parsed document.
/// These properties include meta tags plus evaluated robots instructions.
///
// Historically there was also parse_meta_html5() hence the name.
fn parse_meta_lol_html(bytes: Bytes) -> HashMap<String, String> {
    let mut properties: HashMap<String, String> = HashMap::new();
    let mut text_properties: HashMap<String, String> = HashMap::new();
    let mut noindex = false;

    let mut rewriter = HtmlRewriter::new(
        Settings {
            adjust_charset_on_meta_tag: true,

            element_content_handlers: vec![
                element!("meta", |el| {
                    let property = el.get_attribute("property")
                        .or_else(|| el.get_attribute("name"));

                    let content = el.get_attribute("content");

                    if property.is_some() && content.is_some() {
                        let property = property.unwrap();
                        let content = content.unwrap();

                        // check rule for all robots
                        if property == "robots" {
                            noindex |= cannot_index(&content);
                        }

                        // check rule for fedineko-crabo specifically
                        if property.contains("fedineko-crabo") {
                            noindex |= cannot_index(&content);
                        }

                        properties.insert(
                            property,
                            content,
                        );
                    }

                    Ok(())
                }),
                text!("title", |el| {
                    text_properties.insert(
                        "title".to_string(),
                        el.as_str().to_string()
                    );

                    Ok(())
                }),
            ],

            ..Settings::default()
        },
        |_c: &[u8]| {},
    );

    rewriter.write(&bytes).unwrap_or(());
    rewriter.end().unwrap_or(());

    properties.extend(text_properties);

    properties.insert(
        FEDINEKO_CAN_INDEX_KEY.to_string(),
        (!noindex).to_string()
    );

    properties
}

/// Helper function to parse image URLs passed as `url_str`,
/// including relative to `site_url`.
///
/// There is nothing here that limits it to image URLs parsing only,
/// however no other URLs are collected by Fedineko.
fn parse_image_url(site_url: &Url, url_str: &str) -> Option<Url> {
    match Url::parse(url_str) {
        Ok(url) => return Some(url),

        Err(err) => match err {
            ParseError::RelativeUrlWithoutBase => { /* falling through */ }

            _ => {
                warn!(
                    "{site_url}: Failed to parse '{url_str}' as valid URL: \
                    {err:?}",
                );

                return None;
            }
        }
    }

    match site_url.join(url_str) {
        Ok(url) => Some(url),

        Err(err) => {
            warn!(
                "{site_url}: Failed to both parse anf combine site url and \
                '{url_str}' as valid URL: {err:?}"
            );

            None
        }
    }
}

/// Selects one of multiple possible descriptions in `properties`.
/// Currently, it just selects the longest string.
fn select_description(
    properties: &HashMap<String, String>
) -> Option<&String> {
    let all_descriptions = [
        properties.get("og:description"),
        properties.get("twitter:description"),
        properties.get("description"),
        properties.get("Description"),
    ];

    all_descriptions.into_iter()
        .flatten()
        .sorted_by(|a, b| Ord::cmp(&b.len(), &a.len()))
        .next()
}

/// This functions tries to figure out from meta tags map `properties`
/// if page is likely to contain information related to social services.
/// This is needed to make decision to keep snippet but avoid indexing of it
/// by Plankone as there is no established consent for indexing in such case.
///
/// It happens often when people renote, retoot and other re- of content
/// using text level indicators such as RE: or RN:
///
/// Theoretically speaking, crabo could guess username and try to query server
/// for content details, in practice though it is quite troublesome and
/// error-prone. So Fedineko just skips indexing of such content regardless
/// of consent.
///
/// Another theory is that Oceanhorse, when extracting links from text or
/// attachments, could identify which of those are related to social services.
/// This will require maintaining dynamic list of Fediverse server
/// instances (in fact, could fetch it from existing Fediverse mapping sites).
/// It is doable, however the real issue is that not all social services
/// are ActivityPub based.
///
/// To sum up: if page contains meta tags used by social networking services,
/// Сrabo marks snippet as "guessed.social".
fn guess_social(properties: &HashMap<String, String>) -> Option<&str> {
    let profile_hints = [
        // guessing some Mastodon instances
        properties.get("profile:username"),
        properties.get("og:profile:username"),

        // guessing misskey forks
        properties.get("misskey:user-username"),
        properties.get("misskey:user-id"),
        properties.get("misskey:note-id"),
    ].into_iter()
        .any(|value| value.is_some());

    if profile_hints {
        return Some("guessed.social");
    }

    // Pleroma/Akkoma?

    // Surprisingly, only Misskey family of ActivityPub instances provides
    // usable application-name.
    properties.get("application-name")
        .and_then(|app| match app.to_lowercase().as_str() {
            // See list here: https://trypancakes.com/misskey-comparison/
            "misskey" |
            "sharkey" |
            "foundkey" |
            "iceshrimp" |
            "catodon" |
            "firefish" => Some("guessed.social"),

            _ => None
        })
}

/// This function tries to find enough `properties` to produce some sort
/// of usable snapshot for given `url`. If mime type is not clear from
/// image URL, this function will attempt to guess it by sending HEAD
/// request to server. That is why `client` is provided and function
/// itself is async.
async fn properties_to_snapshot(
    url: Url,
    properties: HashMap<String, String>,
    client: &GenericClient,
) -> Option<Snapshot> {
    if let Some(can_index) = properties.get(FEDINEKO_CAN_INDEX_KEY) {
        match can_index.as_str() {
            "true" => { /* can continue */ }

            _ => {
                info!("{url}: snapshotting is not allowed by meta tags");
                return None;
            }
        }
    }

    let og_title = properties.get("og:title")
        .or_else(|| properties.get("og:site_name"))
        .or_else(|| properties.get("title"))
        .and_then(|s| match s.is_empty() {
            true => None,
            false => Some(s)
        });

    let og_description = select_description(&properties)
        .or(og_title)
        .and_then(|s| match s.is_empty() {
            true => None,
            false => Some(s)
        });

    let og_image = properties.get("og:image")
        .or_else(|| properties.get("twitter:image"));

    let og_site_name = properties.get("og:site_name")
        .or_else(|| properties.get("twitter:site"))
        .or(og_title);

    if og_image.is_none() && og_description.is_none() {
        return None;
    }

    // this could be used by indexer to avoid indexing of pages for
    // particular application. Frontend could present content differently
    // if application is known.
    let application_name = guess_social(&properties)
        .map(|s| s.to_string());

    let preview_url = og_image
        .and_then(|image_url| parse_image_url(&url, image_url));

    let media_type = guess_mime_from_url(preview_url.as_ref(), client).await;

    Some(
        Snapshot {
            url,
            preview_url,
            title: og_title.cloned(),
            description: og_description.cloned(),
            source: og_site_name.cloned(),
            preview_mime_type: media_type.map(|x| x.to_string()),
            tags: vec![],
            application_name,
        }
    )
}

/// Helper method to match URL `parameter` to known campaign tracking names.
/// Some sites allow access to content if URL has no parameters,
/// but deny if it is. Presumably this is to protect dynamically
/// generated content from indexing. Campaign tracking parameters are not
/// parameters for such content.
fn param_matches_utm(parameter: &str) -> bool {
    parameter.starts_with("utm") ||
        parameter.starts_with("amp;amp;utm") ||
        parameter.starts_with("amp;utm") ||
        parameter == "smid" ||
        parameter == "via"
}

/// This function removes query parameters from given `url`.
fn remove_known_campaign_tracking_parameters(mut url: Url) -> Url {
    let original_params_count = url.query_pairs().count();
    let original_url = url.to_string();

    let params: Vec<_> = url.query_pairs()
        .filter(|(param, _)| !param_matches_utm(param))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    if original_params_count != params.len() {
        if params.is_empty() {
            url.set_query(None);
        } else {
            url.query_pairs_mut()
                .clear()
                .extend_pairs(params)
                .finish();
        }

        info!(
            "Filtered campaign tracking parameters so '{original_url}' \
            became '{url}'"
        );
    }

    url
}

impl Snapper for HtmlMetaSnapper {
    fn cache_hints(&self, url: &Url) -> Option<CacheHints> {
        Some(
            CacheHints {
                provider: "default".to_string(),
                id: url.to_string(),
            }
        )
    }

    async fn snap(
        &self,
        original_url: Url,
        cache_hints: CacheHints,
        clients: &Clients
    ) -> SnapshotAndHints {
        let id = &cache_hints.id;

        let url = remove_known_campaign_tracking_parameters(
            original_url.clone()
        );

        if !self.robots_validator.can_access_url(&url, clients).await {
            info!("Access to {url} is disallowed by robots.txt");

            return SnapshotAndHints {
                snapshot: None,
                hints: cache_hints,
            };
        }

        let extra_headers = vec![
            // TODO: add more Sec-Fetch-*?
            //
            // I am in doubts whether referrer should be passed.
            // - Upside is: server knows that Crabo is not randomly scrapping site.
            // - Downside is: it kinda violates privacy of person who added URL
            //   into theirs ActivityPub content.
            // ("X-Fediverse-Referrer", url.as_str()),
            ("Sec-Fetch-Dest", "document"),
            ("Sec-Fetch-Site", "none"),
        ].into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let bytes_result = clients.suppressed_client.get_bytes(
            id,
            Some(extra_headers)
        ).await;

        match bytes_result {
            Ok(bytes) => {
                let properties = parse_meta_lol_html(bytes);

                SnapshotAndHints {
                    snapshot: properties_to_snapshot(
                        original_url,
                        properties,
                        &clients.generic_client
                    ).await,

                    hints: cache_hints,
                }
            }

            Err(err) => {
                match err {
                    ClientError::Suppressed => {
                        warn!(
                            "Server for '{id}' is suppressed, \
                            no request was made"
                        );
                    }

                    _ => {
                        warn!("Failed to get '{id}': {err:?}");
                    }
                }

                SnapshotAndHints {
                    snapshot: None,
                    hints: cache_hints,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use crate::snapper::{CacheHints, Clients};
    use crate::html_meta::{HtmlMetaSnapper, select_description};
    use url::Url;
    use fedineko_http_client::{GenericClient, SuppressedClient};
    use proxydon_client::ProxydonClient;
    use crate::html_meta::guess_mime_from_url;
    use crate::robots::RobotsValidator;
    use crate::snapper::Snapper;

    const CRABO_VERSION: &str = "fedineko/crabo-0.2-test";

    #[actix_rt::test]
    async fn test_fallback_to_head() {
        let client = GenericClient::new_with_user_agent(CRABO_VERSION);

        // TODO: need some stable link.
        let url = Url::parse(
            "https://i.scdn.co/image/ab67616d0000b27358d4b67b2616cb84e0abd3e2"
        ).unwrap();

        let opt_url = Option::from(&url);

        let mime_type = guess_mime_from_url(opt_url, &client).await;

        assert_eq!(mime_type, Some("image/jpeg".to_string()));
    }

    #[actix_rt::test]
    async fn test_encoding_of_values_is_valid() {
        let url = Url::parse(
            "https://www.oricon.co.jp/news/2315448/full/"
        ).unwrap();

        let snapper = HtmlMetaSnapper {
            robots_validator: RobotsValidator::new("test-agent")
        };

        let cache_hints = CacheHints {
            provider: "default".to_string(),
            id: url.to_string(),
        };

        let proxydon_url = url::Url::parse("http://127.0.0.1").unwrap();

        let clients = Clients {
            proxydon_client: ProxydonClient::new(&proxydon_url),
            generic_client: GenericClient::new_with_user_agent(CRABO_VERSION),
            // this one is not actually no follow client, but it is fine
            // in this test.
            no_follow_client: GenericClient::new_with_user_agent(CRABO_VERSION),

            suppressed_client: SuppressedClient::new(
                GenericClient::new_with_user_agent(CRABO_VERSION),
            ),
        };

        let snapshot_and_hints = snapper.snap(
            url,
            cache_hints,
            &clients,
        ).await;

        assert!(snapshot_and_hints.snapshot.is_some());

        let snapshot = snapshot_and_hints.snapshot.unwrap();

        println!("{:?}", snapshot.title);
        println!("{:?}", snapshot.description);
    }

    #[test]
    fn test_description_selection() {
        let properties: HashMap<_, _> = HashMap::from([
            ("og:description", "abc"),
            ("twitter:description", "abcdef"),
            ("description", "defg"),
        ]).into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        assert_eq!(
            select_description(&properties),
            properties.get("twitter:description")
        );
    }
}