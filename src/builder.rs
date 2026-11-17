#[cfg(feature = "audit-core")]
use crate::audit::{AuditLayer, Auditor};
use crate::cookies::CookieJar;
use crate::req::ReqwestService;
use crate::retry::RetryPolicy;
use crate::types::APIClientError;
use crate::{APIClient, ClientInner};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower::util::BoxCloneSyncService;
#[cfg(feature = "audit-core")]
use tracing::debug;

/// Builder for [`APIClient`].
#[derive(Clone)]
pub struct APIClientBuilder {
    pub(crate) base_url: String,
    pub(crate) timeout_secs: u64,
    pub(crate) max_concurrent: Option<usize>,
    pub(crate) pool_max_idle_per_host: Option<usize>,
    pub(crate) connect_timeout_secs: u64,
    pub(crate) read_timeout: Option<Duration>,
    pub(crate) http1_only: bool,
    pub(crate) http2_connection_window: Option<u32>,
    pub(crate) queue_deadline: Option<Duration>,
    pub(crate) retry: RetryPolicy,
    pub(crate) hold_permit_until_body_end: bool,
    #[cfg(test)]
    pub(crate) root_certificates: Vec<reqwest::Certificate>,
    #[cfg(feature = "audit-core")]
    pub(crate) auditor: Option<Arc<dyn Auditor>>,
    /// `None`: not set, so twice `max_concurrent`.
    #[cfg(feature = "audit-core")]
    #[allow(clippy::option_option)]
    pub(crate) audit_write_limit: Option<Option<usize>>,
    #[cfg(feature = "audit-core")]
    pub(crate) audit_body_limit: u64,
}

impl APIClientBuilder {
    /// Create a new builder with default settings:
    /// - `timeout_secs`: 60
    /// - `connect_timeout_secs`: 10
    /// - `max_concurrent`: 10
    /// - `pool_max_idle_per_host`: `max_concurrent` (unlimited without one)
    /// - HTTP/2 is negotiated through ALPN, with a connection window of
    ///   `(max_concurrent + 2) * 2 MiB`
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout_secs: 60,
            max_concurrent: Some(10),
            pool_max_idle_per_host: None,
            connect_timeout_secs: 10,
            read_timeout: None,
            http1_only: false,
            http2_connection_window: None,
            queue_deadline: None,
            retry: RetryPolicy::default(),
            hold_permit_until_body_end: false,
            #[cfg(test)]
            root_certificates: Vec::new(),
            #[cfg(feature = "audit-core")]
            auditor: None,
            #[cfg(feature = "audit-core")]
            audit_write_limit: None,
            #[cfg(feature = "audit-core")]
            audit_body_limit: 4 * 1024 * 1024,
        }
    }

    /// Set the default total time limit of a request in seconds, from sending
    /// it to the end of the body. A request can override it with
    /// [`RequestBuilder::timeout`](crate::RequestBuilder::timeout) or
    /// [`RequestBuilder::no_timeout`](crate::RequestBuilder::no_timeout).
    #[must_use]
    pub const fn timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Set the maximum number of concurrent requests.
    /// Use `None` to disable concurrency limiting.
    #[must_use]
    pub const fn max_concurrent(mut self, max_concurrent: Option<usize>) -> Self {
        self.max_concurrent = max_concurrent;
        self
    }

    /// Set the maximum number of idle connections per host in the pool.
    #[must_use]
    pub const fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.pool_max_idle_per_host = Some(max);
        self
    }

    /// Set the connect timeout in seconds. Defaults to 10.
    #[must_use]
    pub const fn connect_timeout_secs(mut self, connect_timeout_secs: u64) -> Self {
        self.connect_timeout_secs = connect_timeout_secs;
        self
    }

    /// Set a read timeout: the time to the first byte of the response and the
    /// longest gap allowed between body chunks. Off by default.
    #[must_use]
    pub const fn read_timeout(mut self, read_timeout: Duration) -> Self {
        self.read_timeout = Some(read_timeout);
        self
    }

    /// Use HTTP/1.1 only. By default HTTP/2 is negotiated through ALPN when
    /// the server offers it.
    #[must_use]
    pub const fn http1_only(mut self) -> Self {
        self.http1_only = true;
        self
    }

    /// Set the HTTP/2 connection-level flow-control window in bytes.
    ///
    /// The default is `(max_concurrent + 2) * 2 MiB`, at most `2^31 - 1`; with
    /// no concurrency limit and no override the HTTP/2 default is kept.
    /// Ignored with [`Self::http1_only`].
    #[must_use]
    pub const fn http2_connection_window(mut self, bytes: u32) -> Self {
        self.http2_connection_window = Some(bytes);
        self
    }

    /// Give up waiting for a concurrency permit after `deadline`: the request
    /// then fails with [`APIClientError::QueueTimeout`] and is not sent. Off by
    /// default, so a request waits as long as it takes.
    #[must_use]
    pub const fn queue_deadline(mut self, deadline: Duration) -> Self {
        self.queue_deadline = Some(deadline);
        self
    }

    /// Set the retry policy. The default is [`RetryPolicy::default`]; use
    /// [`RetryPolicy::none`] to never retry.
    #[must_use]
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Hold the concurrency permit until the response body has been read to its
    /// end (or the response is dropped), instead of releasing it when the
    /// response head arrives. Off by default; a request can opt in with
    /// [`RequestBuilder::hold_permit_until_body_end`](crate::RequestBuilder::hold_permit_until_body_end).
    ///
    /// Deadlock hazard: a task that holds `max_concurrent` responses with unread
    /// bodies and then awaits another request on the same client waits for
    /// itself. Read or drop a response before starting the next request.
    #[must_use]
    pub const fn hold_permit_until_body_end(mut self, hold: bool) -> Self {
        self.hold_permit_until_body_end = hold;
        self
    }

    /// Trust an extra root certificate; for tests only.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn add_root_certificate(mut self, cert: reqwest::Certificate) -> Self {
        self.root_certificates.push(cert);
        self
    }

    /// Set the auditor for request logging.
    #[cfg(feature = "audit-core")]
    #[must_use]
    pub fn with_auditor(mut self, auditor: Arc<dyn Auditor>) -> Self {
        debug!("API client auditor is set");
        self.auditor = Some(auditor);
        self
    }

    /// Limit how many audit records are written at once, across all calls of
    /// the client. The default is twice `max_concurrent`; `None` is no limit.
    /// A response body stored with
    /// [`AuditConfig::tee_response`](crate::AuditConfig::tee_response) is not
    /// counted, only its `response_partial` record is.
    /// A write waits for a slot without holding a concurrency permit.
    #[cfg(feature = "audit-core")]
    #[must_use]
    pub const fn audit_write_limit(mut self, limit: Option<usize>) -> Self {
        self.audit_write_limit = Some(limit);
        self
    }

    /// Set the largest response body, in bytes, that is read whole to be
    /// audited. The default is 4 MiB.
    ///
    /// The decision is made from the response head: a body with a
    /// `Content-Length` up to the limit is buffered and audited as received. A
    /// longer one, or one without a `Content-Length`, is not buffered: the
    /// caller gets it as a stream, and the audit stores a head-only record
    /// (status, headers without credentials, and a note of the length), or the
    /// whole body when the request asked for
    /// [`AuditConfig::tee_response`](crate::AuditConfig::tee_response).
    ///
    /// A response that reqwest decompresses (`gzip`, `br`, `zstd`, `deflate`)
    /// has no `Content-Length` once decoded, so it is never buffered.
    #[cfg(feature = "audit-core")]
    #[must_use]
    pub const fn audit_body_limit(mut self, bytes: u64) -> Self {
        self.audit_body_limit = bytes;
        self
    }

    /// The idle connections kept per host: the explicit setting, else the
    /// concurrency limit.
    pub(crate) fn effective_pool_max_idle_per_host(&self) -> Option<usize> {
        self.pool_max_idle_per_host.or(self.max_concurrent)
    }

    /// The HTTP/2 connection window to apply, if any.
    pub(crate) fn effective_http2_connection_window(&self) -> Option<u32> {
        if self.http1_only {
            return None;
        }
        self.http2_connection_window.or_else(|| {
            self.max_concurrent.map(|n| {
                let window = (n as u64).saturating_add(2).saturating_mul(2 * 1024 * 1024);
                u32::try_from(window.min(i32::MAX as u64)).unwrap_or(i32::MAX as u32)
            })
        })
    }

    /// Build the [`APIClient`].
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client fails to initialize.
    pub fn build(self) -> Result<APIClient, APIClientError> {
        let cookies = Arc::new(CookieJar::new());
        let mut client_builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(self.connect_timeout_secs))
            .cookie_provider(cookies.clone());

        if let Some(read_timeout) = self.read_timeout {
            client_builder = client_builder.read_timeout(read_timeout);
        }

        if let Some(max) = self.effective_pool_max_idle_per_host() {
            client_builder = client_builder.pool_max_idle_per_host(max);
        }

        if self.http1_only {
            client_builder = client_builder.http1_only();
        } else if let Some(window) = self.effective_http2_connection_window() {
            client_builder = client_builder.http2_initial_connection_window_size(window);
        }

        #[cfg(test)]
        for cert in &self.root_certificates {
            client_builder = client_builder.add_root_certificate(cert.clone());
        }

        let client = client_builder.build()?;

        #[cfg(feature = "audit-core")]
        let audit_writes = self
            .audit_write_limit
            .unwrap_or_else(|| self.max_concurrent.map(|n| n.saturating_mul(2)))
            .map(|n| Arc::new(Semaphore::new(n.max(1))));
        #[cfg(feature = "audit-core")]
        let audit_tasks = Arc::new(crate::audit::Tracker::default());
        let base = ReqwestService::new(client, Some(Duration::from_secs(self.timeout_secs)));
        #[cfg(feature = "audit-core")]
        let svc = ServiceBuilder::new()
            .layer(AuditLayer::new(
                cookies.clone(),
                self.auditor.clone(),
                audit_writes,
                self.audit_body_limit,
                audit_tasks.clone(),
            ))
            .service(base);
        #[cfg(not(feature = "audit-core"))]
        let svc = ServiceBuilder::new().service(base);
        let service = BoxCloneSyncService::new(svc);

        let inner = Arc::new(ClientInner {
            service,
            semaphore: self.max_concurrent.map(|n| Arc::new(Semaphore::new(n))),
            cookies,
            retry: self.retry,
            queue_deadline: self.queue_deadline,
            hold_permit_default: self.hold_permit_until_body_end,
            #[cfg(feature = "audit-core")]
            has_auditor: self.auditor.is_some(),
            #[cfg(feature = "audit-core")]
            audit_tasks,
        });

        Ok(APIClient {
            base_url: self.base_url,
            inner,
        })
    }
}
