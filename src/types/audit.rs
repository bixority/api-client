use crate::types::Method;
use chrono::Utc;
use uuid::Uuid;

/// Configuration for auditing a single request.
#[derive(Clone, Debug)]
pub struct AuditConfig {
    /// Optional name for this audit entry.
    pub name: Option<String>,
    /// Whether to audit the response body. If false, the response can be streamed.
    pub audit_response_body: bool,
}

impl AuditConfig {
    /// Create a new audit configuration with the given name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            audit_response_body: true,
        }
    }

    /// Disable auditing of the response body, allowing it to be streamed.
    #[must_use]
    pub const fn mute_response(mut self) -> Self {
        self.audit_response_body = false;
        self
    }
}

/// Metadata for auditing a request/response pair.
#[derive(Clone, Debug)]
pub struct AuditMetadata {
    pub date_path: String,
    pub timestamp: String,
    pub request_id: String,
    pub uri_path: String,
    pub audit_name: String,
}

impl AuditMetadata {
    #[must_use]
    pub fn new(audit_name: &str, uri: &str) -> Self {
        let now = Utc::now();
        let request_id = Uuid::new_v4().to_string();
        let request_id = request_id[24..].to_owned();

        Self {
            date_path: now.format("%Y/%m/%d").to_string(),
            timestamp: now.format("%y%m%d_%H%M%S").to_string(),
            request_id,
            uri_path: uri.trim_matches('/').to_owned(),
            audit_name: audit_name.to_owned(),
        }
    }

    #[must_use]
    pub fn path(&self, method: Method, suffix: &str) -> String {
        format!(
            "{}/{}/{}/{}_{}_{}_{}.txt",
            self.date_path,
            self.audit_name,
            self.uri_path,
            method,
            self.timestamp,
            self.request_id,
            suffix
        )
    }
}
