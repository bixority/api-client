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
    /// Whether a response body that is too large to buffer is stored while the
    /// caller streams it. See [`Self::tee_response`].
    pub tee_response: bool,
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
            tee_response: false,
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

    /// Store a response body that is too large to buffer (longer than the
    /// client's `audit_body_limit`, or of unknown length such as a chunked or
    /// decompressed one) as the caller streams it, instead of a head-only
    /// record. The body goes to the auditor through a bounded channel of 8
    /// chunks, so a slow auditor slows the caller. If the body ends early (the
    /// caller drops it, or it fails), nothing is stored under `response`; a
    /// `response_partial` record notes how much was seen. Use
    /// [`APIClient::flush_audit`](crate::APIClient::flush_audit) to wait for the
    /// writes before a shutdown.
    #[must_use]
    pub const fn tee_response(mut self) -> Self {
        self.tee_response = true;
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
            tee_response: false,
        }
    }
}

/// Hex digits of the request id: the tail of a random UUID, the same digits
/// that end its hyphenated form (its last group).
const REQUEST_ID_LEN: usize = 12;

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
        let mut buf = Uuid::encode_buffer();
        let hex = Uuid::new_v4().simple().encode_lower(&mut buf);
        let request_id = hex[hex.len() - REQUEST_ID_LEN..].to_owned();

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
        use std::fmt::Write as _;

        let root = self
            .root_path
            .as_deref()
            .map(|r| r.trim_matches('/'))
            .filter(|r| !r.is_empty());
        let cap = root.map_or(0, |r| r.len() + 1)
            + self.date_path.len()
            + self.audit_name.len()
            + self.uri_path.len()
            + self.timestamp.len()
            + self.request_id.len()
            + suffix.len()
            + 32;
        let mut out = String::with_capacity(cap);
        if let Some(root) = root {
            out.push_str(root);
            out.push('/');
        }
        out.push_str(&self.date_path);
        out.push('/');
        out.push_str(&self.audit_name);
        out.push('/');
        // Empty URI segments (a request to `/`) are skipped: an empty object
        // path segment is refused by the object storage client.
        for segment in self.uri_path.split('/').filter(|s| !s.is_empty()) {
            out.push_str(segment);
            out.push('/');
        }
        let _ = write!(
            out,
            "{method}_{}_{}_{suffix}.txt",
            self.timestamp, self.request_id
        );
        out
    }
}
