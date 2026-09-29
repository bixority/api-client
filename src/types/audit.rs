use crate::types::Method;
use chrono::Utc;
use uuid::Uuid;

/// Configuration for auditing a single request.
#[derive(Clone, Debug)]
pub struct AuditConfig {
    /// Optional name for this audit entry.
    pub name: Option<String>,
    /// Optional root path for audit storage.
    pub path: Option<String>,
    /// Whether to audit the response body. If false, the response can be streamed.
    pub audit_response_body: bool,
}

impl AuditConfig {
    /// Create a new audit configuration with the given name.
    ///
    /// The audit path is initialized from the `API_CLIENT_AUDIT_PATH` environment variable if set.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            path: std::env::var("API_CLIENT_AUDIT_PATH").ok(),
            audit_response_body: true,
        }
    }

    /// Set the root path for audit storage.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Set the root path for audit storage.
    #[must_use]
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Set the name for this audit entry.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Disable auditing of the response body, allowing it to be streamed.
    #[must_use]
    pub const fn mute_response(mut self) -> Self {
        self.audit_response_body = false;
        self
    }
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            name: None,
            path: std::env::var("API_CLIENT_AUDIT_PATH").ok(),
            audit_response_body: true,
        }
    }
}

/// Metadata for auditing a request/response pair.
#[derive(Clone, Debug)]
pub struct AuditMetadata {
    pub root_path: Option<String>,
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
            root_path: None,
            date_path: now.format("%Y/%m/%d").to_string(),
            timestamp: now.format("%y%m%d_%H%M%S").to_string(),
            request_id,
            uri_path: uri.trim_matches('/').to_owned(),
            audit_name: audit_name.to_owned(),
        }
    }

    #[must_use]
    pub fn with_root_path(mut self, root_path: Option<String>) -> Self {
        self.root_path = root_path;
        self
    }

    #[must_use]
    pub fn path(&self, method: Method, suffix: &str) -> String {
        let file_path = format!(
            "{}/{}/{}/{}_{}_{}_{}.txt",
            self.date_path,
            self.audit_name,
            self.uri_path,
            method,
            self.timestamp,
            self.request_id,
            suffix
        );

        match &self.root_path {
            Some(root) => {
                let trimmed = root.trim_matches('/');
                if trimmed.is_empty() {
                    file_path
                } else {
                    format!("{trimmed}/{file_path}")
                }
            }
            None => file_path,
        }
    }
}
