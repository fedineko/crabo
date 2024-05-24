use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use actix_web::http::StatusCode;
use chrono::Duration;
use log::{info, warn};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use texting_robots::Robot;
use fedineko_http_client::ClientError;
use proxydon_cache::typed_cache::TypedCache;
use crate::snapper::Clients;

/// Status of robots.txt
#[derive(Clone, Serialize, Deserialize)]
enum RobotsTxtStatus {
    /// robots.txt was fetched successfully before and is available.
    Acquired,

    /// robots.txt was requested before, but it does not exist,
    /// so the only way to figure out if page snapshot could be done
    /// is to check per page meta-tags, if any.
    RequestedNotFound,

    /// robots.txt was requested but fetch failed for any reason.
    RequestedFailed,

    /// robots.txt was not requested before.
    NotRequested,
}

/// This structure encapsulates information about server provided
/// robots.txt definitions.
#[derive(Clone, Serialize, Deserialize)]
struct ServerIndexingPermissions {
    pub robots_txt: Option<String>,
    // TODO: maybe Rc?
    pub robots_txt_status: RobotsTxtStatus,
}

impl ServerIndexingPermissions {
    /// Constructs new instance of [ServerIndexingPermissions] with robots.txt
    /// read from given `data` string.
    pub fn from_string(data: String) -> Self {
        Self {
            robots_txt_status: RobotsTxtStatus::Acquired,
            robots_txt: Some(data),
        }
    }

    /// Constructs new instance of [ServerIndexingPermissions] with
    /// `robots_txt_status` as status of file fetch. Usually it is
    /// used to construct "robots txt is not available" instance of
    /// this struct.
    pub fn new(robots_txt_status: RobotsTxtStatus) -> Self {
        Self {
            robots_txt_status,
            robots_txt: None,
        }
    }
}

/// This struct keeps cache of robots.txt to avoid unnecessary queries
/// to servers and provides methods to validate permission to access page.
pub(crate) struct RobotsValidator {
    user_agent: String,
    robots_txt_permissions: TypedCache<ServerIndexingPermissions>,
    robots_cache: Mutex<LruCache<String, Robot>>,
}

impl RobotsValidator {
    /// This method constructs new instance of [RobotsValidator].
    /// `user_agent` is used to identify Crabo in HTTP requests.
    pub fn new(user_agent: &str) -> Self {
        Self {
            user_agent: user_agent.to_string(),
            // robots_txt content cache
            robots_txt_permissions: TypedCache::new(
                "robots_txt_permissions",
                Some(512),
                // TODO: make these two parameters below configurable
                // will keep it in remote cache for one day
                Duration::try_days(1),
                // will keep in local cache for a couple of hours
                Duration::try_hours(2),
            ),
            // actual matchers
            robots_cache: Mutex::new(LruCache::new(NonZeroUsize::new(256).unwrap())),
        }
    }
    /// Helper method to download robots.txt from `site`.
    /// `url` is provided to figure out whether server requires TLS
    /// or plain HTTP requests.
    /// `clients` is HTTP clients passed around Crabo.
    /// Returns either instance of [ServerIndexingPermissions] or None.
    async fn download_robots_txt(
        &self,
        site: &str,
        url: &url::Url,
        clients: &Clients,
    ) -> Option<ServerIndexingPermissions> {
        info!("Requested robots.txt for {site}");

        let robots_address = format!("{}://{site}/robots.txt", url.scheme());
        let robots_url = url::Url::parse(&robots_address).unwrap();

        match clients.generic_client.get_bytes(&robots_url, None).await {
            Ok(bytes) => {
                match String::from_utf8(bytes.into()) {
                    Ok(data) => Some(
                        ServerIndexingPermissions::from_string(data)
                    ),

                    Err(err) => {
                        warn!(
                            "Failed to read robots.txt for {site}, \
                            treating it as not permissive policy: {err:?}"
                        );

                        Some(
                            ServerIndexingPermissions::new(
                                RobotsTxtStatus::RequestedFailed
                            )
                        )
                    }
                }
            }

            Err(err) => {
                match err {
                    ClientError::UnexpectedStatusCode(status) => {
                        match status {
                            // if status code is 404 then we are allowed
                            // to access any URL
                            StatusCode::NOT_FOUND |
                            // some servers return 403 for non-existent files,
                            // so treating it as 404.
                            StatusCode::FORBIDDEN
                            // some also return 400... but let's ignore it.
                            => Some(
                                ServerIndexingPermissions::new(
                                    RobotsTxtStatus::RequestedNotFound
                                )
                            ),

                            _ => Some(ServerIndexingPermissions::new(
                                RobotsTxtStatus::RequestedFailed)
                            )
                        }
                    }
                    ClientError::Suppressed => {
                        warn!(
                            "Requests to server for {robots_address} \
                            are suppressed"
                        );

                        // requests are suppressed, regardless of robots.txt settings nothing to do.
                        None
                    }
                    _ => {
                        // failed to fetch, it should be cached
                        warn!("Failed to fetch {robots_address}: {err:?}");
                        Some(
                            ServerIndexingPermissions::new(
                                RobotsTxtStatus::RequestedFailed
                            )
                        )
                    }
                }
            }
        }
    }

    /// This helper method checks if cached robots.txt for `site` exists
    /// in cache. `clients` provide Proxydon client.
    async fn get_permissions_from_cache(
        &self,
        site: String,
        clients: &Clients,
    ) -> Option<ServerIndexingPermissions> {
        let mut result = self.robots_txt_permissions.get(
            vec![site.clone()],
            &clients.proxydon_client,
        ).await;

        result.remove(&site)
            .flatten()
    }

    /// This helper methods puts acquired server indexing `permissions` object
    /// for `site` into cache. `clients` provide Proxydon client,
    async fn put_permissions_to_cache(
        &self,
        site: String,
        permissions: ServerIndexingPermissions,
        clients: &Clients,
    ) {
        self.robots_txt_permissions.put(
            HashMap::from(
                [(site, permissions.clone())]
            ),
            &clients.proxydon_client,
        ).await;
    }

    /// This helper methods returns earluir acquired server indexing permissions object
    /// for `site` and `url`. `clients` provide Proxydon client,
    async fn get_cached_permissions(
        &self,
        site: String,
        url: &url::Url,
        clients: &Clients,
    ) -> ServerIndexingPermissions {
        let permissions = self.get_permissions_from_cache(
            site.clone(),
            clients,
        ).await;

        // negative hit should not happen here as cache is populated
        // on negative result, so None means no any data stored.
        match permissions {
            None => match self.download_robots_txt(&site, url, clients).await {
                // if still None, then server is suppressed,
                // meaning cannot access server regardless of robots.txt
                None => ServerIndexingPermissions::new(
                    RobotsTxtStatus::NotRequested
                ),

                Some(permissions) => {
                    // cache it
                    self.put_permissions_to_cache(
                        site,
                        permissions.clone(),
                        clients,
                    ).await;

                    permissions
                }
            }

            Some(permissions) => permissions
        }
    }

    /// This method returns `true` if `url` is allowed to be read according to
    /// earlier acquired `permissions` for site.
    fn check_acquired_permissions(
        &self,
        site: String,
        url: &url::Url,
        permissions: ServerIndexingPermissions,
    ) -> bool {
        let robots_content = permissions.robots_txt.unwrap();

        match Robot::new(&self.user_agent, robots_content.as_bytes()) {
            Ok(robot) => {
                let result = robot.allowed(url.as_str());

                let mut robots_cache = self.robots_cache.lock()
                    .unwrap();

                robots_cache.put(site, robot);
                result
            }

            Err(err) => {
                warn!(
                    "Failed to parse robots.txt for {site}, \
                    assuming no access: {err:?}"
                );

                false
            }
        }
    }

    /// This method returns `true` if `url` is allowed to be read according to
    /// cached robots.txt data for site content `url` points to.
    /// `clients` provides HTTP and Proxydon clients used under the hood.
    pub async fn can_access_url(
        &self,
        url: &url::Url,
        clients: &Clients,
    ) -> bool {
        let site = url.host();

        if site.is_none() {
            warn!(
                "Invalid URL passed to robots.txt permissions validator: {url}"
            );

            return false;
        }

        let site = site.unwrap().to_string();

        // scoping mutex guard
        {
            let mut robots_cache = self.robots_cache.lock().unwrap();

            // first check cache of matchers
            match robots_cache.get(&site) {
                Some(robot) => return robot.allowed(url.as_str()),

                None => { /* no matcher in cache */ }
            }
        }

        // check if robots.txt is cached
        let permissions = self.get_cached_permissions(
            site.clone(),
            url,
            clients,
        ).await;

        match permissions.robots_txt_status {
            RobotsTxtStatus::Acquired => self.check_acquired_permissions(
                site,
                url,
                permissions,
            ),

            RobotsTxtStatus::RequestedNotFound => {
                true
            }

            RobotsTxtStatus::RequestedFailed |
            RobotsTxtStatus::NotRequested => {
                false
            }
        }
    }
}
