use url::Url;
use fedineko_http_client::GenericClient;

pub(crate) const CRABO_VERSION: &str = "0.3.1";

/// Guesses content type for resource identified by `url`.
/// If guessing by file extension fails, request to resources
/// is performed with given `client`.
pub(crate) async fn guess_mime_from_url(
    url: Option<&Url>,
    client: &GenericClient
) -> Option<String> {
    url?;
    fedineko_url_utils::guess_mime_type_from_url(url.unwrap(), client).await
}