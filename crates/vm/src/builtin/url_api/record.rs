//! Stable snapshot encoding for the upstream constructor input.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(remote = "urlpattern::UrlPatternInit")]
pub(crate) struct PatternInit {
    protocol: Option<String>,
    username: Option<String>,
    password: Option<String>,
    hostname: Option<String>,
    port: Option<String>,
    pathname: Option<String>,
    search: Option<String>,
    hash: Option<String>,
    base_url: Option<url::Url>,
}
