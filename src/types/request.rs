use crate::types::Method;
#[cfg(feature = "audit-core")]
use crate::types::{AuditConfig, AuditMetadata};
use reqwest::header::HeaderMap;
use std::time::Duration;
use tokio::sync::OwnedSemaphorePermit;

/// The total time limit of one request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestTimeout {
    /// The client's `timeout_secs`.
    Default,
    /// This limit instead.
    After(Duration),
    /// No limit.
    None,
}

/// Internal request representation passed through the Tower service stack.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub(crate) method: Method,
    pub(crate) url: url::Url,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Option<bytes::Bytes>,
    pub(crate) timeout: RequestTimeout,
    pub(crate) hold_permit: bool,
    #[cfg(feature = "audit-core")]
    pub(crate) audit: Option<AuditConfig>,
    #[cfg(feature = "audit-core")]
    pub(crate) audit_meta: Option<AuditMetadata>,
}

/// What the service stack is called with: the request and the concurrency
/// permit acquired for it.
pub struct Call {
    pub(crate) req: HttpRequest,
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    /// How long the permit took to acquire; read by the audit layer.
    #[cfg_attr(not(feature = "audit-core"), allow(dead_code))]
    pub(crate) permit_wait: Duration,
}

/// What the service stack returns: the response and the permit handed back.
pub struct Exchange {
    pub(crate) resp: reqwest::Response,
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    /// Timings and sizes filled by the audit layer, for the per-call event.
    pub(crate) audit: Option<AuditTimings>,
}

/// How the response body of an audited call was audited.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(feature = "audit-core"), allow(dead_code))]
pub enum BodyAudit {
    /// Read whole and stored (or logged) as received.
    Buffered,
    /// Too large or of unknown length: only the head was stored or logged.
    HeadOnly,
    /// Stored while the caller streams it.
    Tee,
}

impl BodyAudit {
    #[cfg_attr(not(feature = "audit-core"), allow(dead_code))]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Buffered => "buffered",
            Self::HeadOnly => "head_only",
            Self::Tee => "tee",
        }
    }
}

/// What the audit layer measured for one call. Milliseconds throughout.
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(not(feature = "audit-core"), allow(dead_code))]
pub struct AuditTimings {
    /// Writing the request record, once the write slot was held.
    pub(crate) request_record_ms: Option<u64>,
    /// From calling the inner service to the response head.
    pub(crate) head_ms: u64,
    /// Reading the response body; `None` when it was not read.
    pub(crate) body_ms: Option<u64>,
    /// Writing the response record, once the write slot was held.
    pub(crate) response_record_ms: Option<u64>,
    pub(crate) request_bytes: u64,
    pub(crate) response_bytes: Option<u64>,
    /// `None` when the response body is muted.
    pub(crate) body: Option<BodyAudit>,
}
