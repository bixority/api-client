use crate::types::Method;
#[cfg(feature = "audit")]
use crate::types::{AuditConfig, AuditMetadata};
use reqwest::header::HeaderMap;

/// Internal request representation passed through the Tower service stack.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Option<Vec<u8>>,
    #[cfg(feature = "audit")]
    pub(crate) audit: Option<AuditConfig>,
    #[cfg(feature = "audit")]
    pub(crate) audit_meta: Option<AuditMetadata>,
}
