use url::Url;
use fedineko_http_client::{GenericClient, SuppressedClient};
use proxydon_client::ProxydonClient;
use crabo_model::Snapshot;

/// Defines interface for site snapshot producers.
pub(crate) trait Snapper {
    /// Returns some [CacheHints] for given `url` if this snapper
    /// could deal with URL.
    fn cache_hints(&self, url: &Url) -> Option<CacheHints>;

    /// This method produces snapshot for `url` and `cache_hints`,
    /// `clients` provide HTTP and Proxydon clients.
    async fn snap(
        &self,
        url: Url,
        cache_hints: CacheHints,
        clients: &Clients
    ) -> SnapshotAndHints;
}

pub(crate) struct Clients {
    /// Cache client.
    pub(crate) proxydon_client: ProxydonClient,

    /// The simplest HTTP client.
    pub(crate) generic_client: GenericClient,

    // Unfortunately awc used under the hood does not expose configuration,
    // so setting it per request is not possible, yet creating new instances
    // of client for each request does not feel quite right.
    /// This client does not follow redirects.
    pub(crate) no_follow_client: GenericClient,

    /// This client knows how to ignore servers that report errors.
    pub(crate) suppressed_client: SuppressedClient,
}

/// This structure is used tp provide hints for snapshotting.
#[derive(Clone)]
pub(crate) struct CacheHints {
    /// Identifies snapper for this hints object.
    pub provider: String,

    /// ID of object, e.g. video ID to pass into some service API client.
    pub id: String,
}


/// Wrapper to pass snapshot and hints together.
pub(crate) struct SnapshotAndHints {
    pub snapshot: Option<Snapshot>,
    pub hints: CacheHints,
}
