//! Host protocol for one HTTP exchange. The VM owns Fetch algorithms, URL list
//! handling, redirects, CORS filtering and content decoding; an adapter owns
//! DNS, connections, TLS and HTTP framing for exactly one hop.
//!
//! Registration is the only way to move bytes: upload reads wait for
//! `UploadCredit`, and the VM grants download credit before the adapter may
//! deliver `Data`. A transaction terminates exactly once.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::binding::{HostFault, OperationToken};

/// Version of the event and registration contract below.
pub const FETCH_PROTOCOL_VERSION: u32 = 1;

/// Network permission and origin policy recorded with the VM.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchEnvironment {
    /// Script origin. `None` is an opaque origin that never matches another.
    pub origin: Option<Origin>,
    /// Absolute referrer, or `None` for no referrer header.
    pub referrer: Option<String>,
    /// Storage/partition identifier forwarded to every hop.
    pub partition: String,
    /// Host policy identifier. Restore only accepts an environment that
    /// authorizes at least the recorded identifier.
    pub authorization: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl Origin {
    pub fn from_url(url: &Url) -> Option<Self> {
        let host = url.host_str()?.to_owned();
        Some(Self {
            scheme: url.scheme().to_owned(),
            host,
            port: url.port_or_known_default()?,
        })
    }

    pub fn same_as(&self, other: &Self) -> bool {
        self.scheme == other.scheme && self.host == other.host && self.port == other.port
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialsMode {
    Omit,
    SameOrigin,
    Include,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadBody {
    /// No request body.
    None,
    /// Body length is known before the exchange starts.
    Known(u64),
    /// Streaming upload; the adapter must not assume a length.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HopRequest {
    pub url: Url,
    pub method: String,
    /// Ordered raw header name and value bytes, already filtered for this hop.
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub upload: UploadBody,
    /// Serialized `Origin` header value when one applies.
    pub origin: Option<String>,
    pub credentials: CredentialsMode,
    pub partition: String,
    pub authorization: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseHead {
    pub status: u16,
    pub status_text: Vec<u8>,
    /// Ordered raw header bytes, never combined or decompressed by the adapter.
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FetchFailure {
    /// Connection, DNS or HTTP framing failure.
    Network(String),
    /// The adapter refused this hop for policy reasons.
    Policy(String),
    /// The adapter could not complete its own work.
    Internal(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FetchEvent {
    /// Additional bytes the VM may read from the upload stream and send.
    UploadCredit(u64),
    /// A 1xx response other than 101; the final response is still pending.
    Informational(ResponseHead),
    /// The final response head. At most once per transaction.
    Head(ResponseHead),
    /// Response body bytes, only after `Head` and within granted credit.
    Data(Vec<u8>),
    /// Response body complete.
    Eof,
    /// Terminal failure, valid before or after `Head`.
    Failed(FetchFailure),
}

/// Starts exactly one hop. The returned registration owns the connection.
pub trait FetchService: Send + Sync {
    fn start(
        self: Arc<Self>,
        token: OperationToken,
        request: HopRequest,
    ) -> Result<Box<dyn FetchRegistration>, HostFault>;
}

/// Optional HTTP exchange adapter.
pub struct FetchKey;

impl crate::ServiceKey for FetchKey {
    type Value = Arc<dyn FetchService>;
    const ID: &'static str = "wintertc.fetch";
    const VERSION: u32 = FETCH_PROTOCOL_VERSION;
    const IN_FINGERPRINT: bool = false;
}

impl std::fmt::Debug for dyn FetchRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FetchRegistration")
    }
}

/// Adapter side of one started hop. Every method returns a fault when the
/// exchange can no longer make progress; `cancel` is idempotent.
pub trait FetchRegistration: Send {
    fn send_upload(&mut self, bytes: Vec<u8>) -> Result<(), HostFault>;
    fn finish_upload(&mut self) -> Result<(), HostFault>;
    fn grant_download(&mut self, bytes: u64) -> Result<(), HostFault>;
    fn cancel(&mut self) -> Result<(), HostFault>;
}
