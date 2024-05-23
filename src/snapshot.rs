use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use chrono::Duration;
use futures::future::join_all;
use log::{debug, info};
use url::Url;
use crabo_model::Snapshot;
use language_utils::content_cleaner::ContentCleaner;
use proxydon_client::cache::ProxydonCache;
use proxydon_client::CacheItem;
use crate::bilibili::BiliBiliSnapper;
use crate::html_meta::HtmlMetaSnapper;
use crate::snapper::{CacheHints, Clients, Snapper, SnapshotAndHints};
use crate::youtube::YoutubeSnapper;

/// This is where all processing logic happens.
pub(crate) struct SnapshotMaker<'a> {
    /// Typeless Proxydon cache instance.
    cache: Arc<ProxydonCache>,

    /// Handy cleaner of html tags and whatnot from titles
    /// and descriptions used in snapshot.
    content_cleaner: ContentCleaner<'a>,

    /// YouTube videos metadata snapper.
    /// It uses official API so needs key for it.
    youtube: YoutubeSnapper,

    /// BiliBili videos metadata snapper.
    /// It uses unofficial API as official does not seem to exist.
    bilibili: BiliBiliSnapper,

    /// General purpose HTML snapper
    html_meta: HtmlMetaSnapper,
}

impl SnapshotMaker<'_> {
    /// This method constructs new instance of [SnapshotMaker]
    /// with `youtube_api_key` for YouTube snapper.
    pub(crate) fn new(youtube_api_key: String) -> Self {
        Self {
            cache: Arc::new(ProxydonCache::new(
                "thumbnail",
                None,
            )),

            youtube: YoutubeSnapper::new(youtube_api_key),
            content_cleaner: ContentCleaner::new(),
            bilibili: BiliBiliSnapper {},
            html_meta: HtmlMetaSnapper::new(),
        }
    }

    /// This method selects one of snappers that could snap `url`.
    /// If special ones are not applicable, general purpose HTML
    /// snapper is hinted.
    fn cache_hints(&self, url: &Url) -> CacheHints {
        if let Some(hints) = self.youtube.cache_hints(url) {
            return hints;
        }

        if let Some(hints) = self.bilibili.cache_hints(url) {
            return hints;
        }

        CacheHints {
            provider: "default".into(),
            id: url.to_string(),
        }
    }

    /// This method does a lousy unescaping of `text` string.
    /// `\n` becomes `<br />`
    /// `\\n` becomes `\<br />`
    /// TODO: move to ContentCleaner
    fn unescape_newline_and_clean(&self, text: &str) -> String {
        let with_tags = text.replace('\n', "<br />");
        self.content_cleaner.clean_content(&with_tags, false)
    }

    /// This method performs cleaning of all text fields so `snapshot` data
    /// is somewhat safe to render in HTML page later.
    fn clean_snapshot(
        &self,
        snapshot: Option<Snapshot>,
    ) -> Option<Snapshot> {
        snapshot.map(|snapshot| Snapshot {
            title: snapshot.title.map(
                |title| self.content_cleaner.clean_content(
                    &title,
                    false,
                )
            ),

            description: snapshot.description.map(
                |description| self.unescape_newline_and_clean(&description)
            ),

            source: snapshot.source.map(
                |source| self.content_cleaner.clean_content(&source, false)
            ),

            tags: snapshot.tags.into_iter()
                .map(|tag| self.content_cleaner.clean_content(&tag, false))
                .filter(|tag| !tag.is_empty())
                .collect(),
            ..snapshot
        })
    }

    /// This method updates cache with `snapshot_and_hints` data
    /// to avoid repeated queries for the same page on web-server side.
    /// `clients` provides Proxydon client.
    async fn update_cache_many(
        &self,
        clients: &Clients,
        snapshot_and_hints: Vec<&SnapshotAndHints>
    ) {
        // TODO: make it configurable.
        let expires_at = chrono::Utc::now() + Duration::try_weeks(1).unwrap();
        let local_cache_expires_at = None;

        let items: Vec<_> = snapshot_and_hints.into_iter()
            .map(|sh| {
                match &sh.snapshot {
                    None => CacheItem {
                        id: sh.hints.id.clone(),
                        content: None,
                        expires_at,
                        local_cache_expires_at,
                    },

                    Some(snapshot) => CacheItem {
                        id: sh.hints.id.clone(),
                        content: Some(serde_json::to_string(&snapshot).unwrap()),
                        expires_at,
                        local_cache_expires_at,
                    }
                }
            }).collect();

        self.cache
            .put(items, &clients.proxydon_client)
            .await;
    }

    /// This helper method converts typeless `cache_item` into instance
    /// of [Snapshot].
    fn cache_item_to_snapshot(
        &self,
        cache_item: CacheItem
    ) -> Option<Snapshot> {
        let id = &cache_item.id;

        if cache_item.content.is_none() {
            debug!("Got negative hit for '{id}'");
            return None;
        }

        debug!("Got cached snapshot for '{id}'");

        let content = cache_item.content.unwrap();

        serde_json::from_str(&content).ok()
    }

    /// This method figures out from `cache_hints` which snapper to use
    /// to produce snapshots for `url`. `clients` are used under the hood
    /// to access cache or API.
    async fn snap_with_cache_hints(
        &self,
        url: Url,
        cache_hints: CacheHints,
        clients: &Clients,
    ) -> SnapshotAndHints {
        match cache_hints.provider.as_str() {
            "youtube" => self.youtube.snap(url, cache_hints, clients).await,
            "bilibili" => self.bilibili.snap(url, cache_hints, clients).await,
            "default" => self.html_meta.snap(url, cache_hints, clients).await,

            _ => SnapshotAndHints {
                snapshot: None,
                hints: cache_hints,
            }
        }
    }

    /// Returns true for `url` if site is known to provide useless data
    /// or errors.
    fn ignored_url(&self, url: &Url) -> bool {
        // TODO: Twitch video URLs snapper using Twitch API
        // "twitch.com"
        // "www.twitch.com"
        match url.host() {
            None => true,
            Some(host) => {
                let host_string = host.to_string();
                host_string.ends_with("twitter.com") ||
                    host_string.ends_with(".x.com") ||
                    host_string == "x.com"
            }
        }
    }

    /// This method makes snapshots for multiple `urls` using giving `clients`.
    /// If `bypass_cache` is specified then cached earlier snapshots for URL
    /// are ignored.
    pub(crate) async fn snap_many(
        &self,
        urls: Vec<Url>,
        clients: &Clients,
        bypass_cache: bool,
    ) -> Vec<Snapshot> {
        debug!(
            "Got request to snap {:?}, bypass cache option is {}",
            urls.iter().map(|x| x.as_str()).collect::<Vec<_>>(),
            bypass_cache,
        );

        let hints: HashMap<_, _> = urls.into_iter()
            .filter(|url| {
                let is_ignored = self.ignored_url(url);

                if is_ignored {
                    info!("{url} is ignored");
                }

                !is_ignored
            })
            .map(|url| (self.cache_hints(&url), url))
            .map(|(x, y)| (y, x))
            .collect();

        let ids: Vec<_> = hints.values()
            .map(|cache_hints| cache_hints.id.clone())
            .collect();

        let have_in_cache = match bypass_cache {
            false => self.cache
                .get(ids, &clients.proxydon_client)
                .await,

            true => vec![],
        };

        let have_in_cache_set: HashSet<_> = have_in_cache.iter()
            .map(|x| x.id.as_str())
            .collect();

        let futures_to_await: Vec<_> = hints.into_iter()
            .filter(|(_, cache_hints)| !have_in_cache_set.contains(
                cache_hints.id.as_str()
            ))
            .map(|(url, cache_hints)| self.snap_with_cache_hints(
                url,
                cache_hints,
                clients
            ))
            .collect();

        let just_loaded: Vec<_> = join_all(futures_to_await)
            .await
            .into_iter()
            .map(|sh| SnapshotAndHints {
                snapshot: self.clean_snapshot(sh.snapshot),
                ..sh
            }).collect();

        self.update_cache_many(
            clients,
            just_loaded.iter().collect(),
        ).await;

        let just_loaded_cache_items: Vec<_> = just_loaded.into_iter()
            .filter_map(|x| x.snapshot)
            .collect();

        let have_in_cache_items = have_in_cache.into_iter()
            .filter_map(|item| self.cache_item_to_snapshot(item))
            .collect();

        [
            have_in_cache_items,
            just_loaded_cache_items
        ].into_iter()
            .flatten()
            .collect()
    }
}
