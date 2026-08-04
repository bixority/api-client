use crate::audit::{AuditLayer, Auditor};
use crate::cookies::CookieJar;
use crate::req::ReqwestService;
use crate::types::APIClientError;
use crate::{APIClient, ClientInner};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower::util::BoxCloneSyncService;
use tracing::debug;

/// Builder for [`APIClient`].
#[derive(Clone)]
pub struct APIClientBuilder {
    pub(crate) base_url: String,
    pub(crate) timeout_secs: u64,
    pub(crate) max_concurrent: Option<usize>,
    pub(crate) pool_max_idle_per_host: Option<usize>,
    pub(crate) auditor: Option<Arc<dyn Auditor>>,
}

impl APIClientBuilder {
    /// Create a new builder with default settings:
    /// - `timeout_secs`: 60
    /// - `max_concurrent`: 10
    #[must_use]
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            timeout_secs: 60,
            max_concurrent: Some(10),
            pool_max_idle_per_host: None,
            auditor: None,
        }
    }

    /// Set the request timeout in seconds.
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

    /// Set the auditor for request logging.
    #[must_use]
    pub fn with_auditor(mut self, auditor: Arc<dyn Auditor>) -> Self {
        debug!("API client auditor is set");
        self.auditor = Some(auditor);
        self
    }

    /// Build the [`APIClient`].
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client fails to initialize.
    pub fn build(self) -> Result<APIClient, APIClientError> {
        let timeout = Duration::from_secs(self.timeout_secs);
        let cookies = Arc::new(CookieJar::new());
        let mut client_builder = reqwest::Client::builder()
            .timeout(timeout)
            .cookie_provider(cookies.clone());

        if let Some(max) = self.pool_max_idle_per_host {
            client_builder = client_builder.pool_max_idle_per_host(max);
        }

        let client = client_builder.build()?;

        let base = ReqwestService::new(client);
        let svc = ServiceBuilder::new()
            .layer(AuditLayer::new(cookies.clone(), self.auditor.clone()))
            .service(base);
        let service = BoxCloneSyncService::new(svc);

        let inner = Arc::new(ClientInner {
            service,
            semaphore: self.max_concurrent.map(|n| Arc::new(Semaphore::new(n))),
            cookies,
        });

        Ok(APIClient {
            base_url: self.base_url,
            inner,
        })
    }
}
