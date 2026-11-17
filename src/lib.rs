#[cfg(feature = "audit-core")]
pub mod audit;
pub mod builder;
mod cookies;
mod req;
mod request_builder;
mod retry;
pub mod types;

#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod unit_tests;

#[cfg(feature = "audit")]
pub use crate::audit::ObjectStorageAuditor;
#[cfg(feature = "audit-core")]
pub use crate::audit::{Auditor, BackgroundAuditor, BackgroundOptions};
pub use crate::builder::APIClientBuilder;
pub use crate::request_builder::RequestBuilder;
pub use crate::retry::RetryPolicy;
pub use crate::types::{APIClientError, Headers, HttpResponse, Method, StatusCode};
#[cfg(feature = "audit-core")]
pub use crate::types::{AuditConfig, AuditMetadata};

use crate::cookies::CookieJar;
use crate::retry::parse_retry_after;
use crate::types::{BodyAudit, Call, Exchange, HttpRequest};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tower::ServiceExt;
use tower::util::BoxCloneSyncService;
use tracing::{debug, info};
use url::Url;

#[derive(Clone)]
pub struct APIClient {
    pub base_url: String,
    pub(crate) inner: Arc<ClientInner>,
}

#[derive(Clone)]
pub(crate) struct ClientInner {
    pub(crate) service: BoxCloneSyncService<Call, Exchange, APIClientError>,
    pub(crate) semaphore: Option<Arc<Semaphore>>,
    pub(crate) cookies: Arc<CookieJar>,
    pub(crate) retry: RetryPolicy,
    pub(crate) queue_deadline: Option<Duration>,
    pub(crate) hold_permit_default: bool,
    /// Whether an auditor is set: only then is `AuditMetadata` built.
    #[cfg(feature = "audit-core")]
    pub(crate) has_auditor: bool,
    #[cfg(feature = "audit-core")]
    pub(crate) audit_tasks: Arc<crate::audit::Tracker>,
}

impl APIClient {
    /// Create a new [`APIClientBuilder`] with default settings.
    #[allow(clippy::new_ret_no_self)]
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> APIClientBuilder {
        APIClientBuilder::new(base_url)
    }

    /// Create a new [`APIClientBuilder`] with default settings.
    #[must_use]
    pub fn builder(base_url: impl Into<String>) -> APIClientBuilder {
        APIClientBuilder::new(base_url)
    }

    /// Start a `GET` request to `base_url + uri`.
    pub fn get(&self, uri: &str) -> RequestBuilder<'_> {
        self.req(Method::Get, uri)
    }

    /// Start a `POST` request to `base_url + uri`.
    pub fn post(&self, uri: &str) -> RequestBuilder<'_> {
        self.req(Method::Post, uri)
    }

    /// Start a `PUT` request to `base_url + uri`.
    pub fn put(&self, uri: &str) -> RequestBuilder<'_> {
        self.req(Method::Put, uri)
    }

    /// Start a `PATCH` request to `base_url + uri`.
    pub fn patch(&self, uri: &str) -> RequestBuilder<'_> {
        self.req(Method::Patch, uri)
    }

    /// Start a `DELETE` request to `base_url + uri`.
    pub fn delete(&self, uri: &str) -> RequestBuilder<'_> {
        self.req(Method::Delete, uri)
    }

    /// Start a `HEAD` request to `base_url + uri`.
    pub fn head(&self, uri: &str) -> RequestBuilder<'_> {
        self.req(Method::Head, uri)
    }

    /// Start a request with `method` to `base_url + uri`.
    pub fn req(&self, method: Method, uri: &str) -> RequestBuilder<'_> {
        RequestBuilder::new(self, method, uri)
    }

    /// Drop every cookie currently held by the underlying HTTP client.
    pub fn clear_cookies(&self) {
        self.inner.cookies.clear();
    }

    /// Wait until every audit write that outlives its call has finished:
    /// the response bodies being stored with
    /// [`AuditConfig::tee_response`], and the `response_partial` records of
    /// those that ended early. Call it before shutting down, or a stream still
    /// being stored is cut off. It waits for nothing else, and returns at once
    /// when no such write is running; one started while it waits is waited
    /// for too.
    #[cfg(feature = "audit-core")]
    pub async fn flush_audit(&self) {
        self.inner.audit_tasks.wait().await;
    }

    pub(crate) fn build_url(
        &self,
        uri: &str,
        query: &[(String, String)],
    ) -> Result<Url, APIClientError> {
        let base = self.base_url.trim_end_matches('/');
        let path = uri.trim_start_matches('/');
        let mut url = Url::parse(&format!("{base}/{path}"))?;

        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in query {
                pairs.append_pair(k, v);
            }
        }
        Ok(url)
    }

    async fn acquire_permit(
        &self,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, APIClientError> {
        let Some(sem) = self.inner.semaphore.as_ref() else {
            return Ok(None);
        };
        let acquire = sem.clone().acquire_owned();
        let permit = match self.inner.queue_deadline {
            Some(deadline) => tokio::time::timeout(deadline, acquire)
                .await
                .map_err(|_| APIClientError::QueueTimeout)?,
            None => acquire.await,
        };
        Ok(Some(permit.map_err(|_| APIClientError::ConcurrencyClosed)?))
    }

    async fn request_once(&self, req: HttpRequest) -> Result<HttpResponse, APIClientError> {
        let queued = Instant::now();
        let permit = self.acquire_permit().await?;
        let permit_wait = queued.elapsed();

        let method = req.method;
        let hold = req.hold_permit;
        #[cfg(feature = "audit-core")]
        let audit_name = req.audit.as_ref().and_then(|a| a.name.clone());
        #[cfg(not(feature = "audit-core"))]
        let audit_name: Option<String> = None;
        let svc = self.inner.service.clone();
        let sent = Instant::now();
        let exchange = svc
            .oneshot(Call {
                req,
                permit,
                permit_wait,
            })
            .await;
        let Exchange {
            resp,
            permit,
            audit,
        } = match exchange {
            Ok(exchange) => exchange,
            Err(err) => {
                debug!(
                    method = %method,
                    permit_wait_ms = permit_wait.as_millis(),
                    head_ms = sent.elapsed().as_millis(),
                    audit = audit_name.as_deref(),
                    error = %err.redacted(),
                    "call"
                );
                return Err(err);
            }
        };
        debug!(
            method = %method,
            status = resp.status().as_u16(),
            version = ?resp.version(),
            permit_wait_ms = permit_wait.as_millis(),
            head_ms = audit.map_or_else(|| sent.elapsed().as_millis(), |a| u128::from(a.head_ms)),
            audit = audit_name.as_deref(),
            request_record_ms = audit.and_then(|a| a.request_record_ms),
            body_ms = audit.and_then(|a| a.body_ms),
            response_record_ms = audit.and_then(|a| a.response_record_ms),
            request_bytes = audit.map(|a| a.request_bytes),
            response_bytes = audit.and_then(|a| a.response_bytes),
            body = audit.and_then(|a| a.body).map(BodyAudit::as_str),
            "call"
        );
        // Without the flag the permit ends here, at the response head.
        let permit = if hold { permit } else { None };
        Ok(HttpResponse::from_reqwest(resp, permit))
    }

    /// Send `req`, retrying under the client's [`RetryPolicy`]. `uri` is only
    /// for the log line.
    pub(crate) async fn execute(
        &self,
        req: HttpRequest,
        uri: &str,
    ) -> Result<HttpResponse, APIClientError> {
        let policy = &self.inner.retry;
        let method = req.method;
        let retryable_method = policy.method_retryable(method);

        let mut attempt = 0;
        loop {
            attempt += 1;
            // A request that cannot be retried, or the last attempt, is moved.
            if !retryable_method || attempt >= policy.max_attempts {
                return self.request_once(req).await;
            }

            let (reason, retry_after): (String, Option<Duration>) =
                match self.request_once(req.clone()).await {
                    Ok(response) if policy.status_retryable(response.status().as_u16()) => (
                        response.status().as_u16().to_string(),
                        response
                            .header("retry-after")
                            .as_deref()
                            .and_then(parse_retry_after),
                    ),
                    Ok(response) => return Ok(response),
                    Err(e) if Self::is_error_retryable(policy, &e) => (e.redacted(), None),
                    Err(e) => return Err(e),
                };

            let delay = policy.delay(attempt, retry_after);
            info!(
                "Retryable {} {} for {} {}, retrying in {}ms...",
                if attempt == 1 { "condition" } else { "error" },
                reason,
                method,
                uri,
                delay.as_millis()
            );
            tokio::time::sleep(delay).await;
        }
    }

    fn is_error_retryable(policy: &RetryPolicy, e: &APIClientError) -> bool {
        match e {
            // reqwest reports a timeout as a request error too, so it is told
            // apart first.
            APIClientError::Request(re) if re.is_timeout() => policy.retry_timeouts,
            APIClientError::Request(re) => re.is_connect() || re.is_request(),
            _ => false,
        }
    }

    /// Execute an HTTP request against `base_url + uri`.
    ///
    /// Query parameters are appended to the URL; the request is bounded by the
    /// optional concurrency semaphore configured via [`Self::new`].
    ///
    /// Requests are retried under the client's [`RetryPolicy`].
    ///
    /// # Errors
    ///
    /// Returns an error if the URL cannot be parsed, the semaphore is closed,
    /// or the underlying HTTP call fails.
    #[cfg(feature = "audit-core")]
    #[deprecated(
        since = "0.2.0",
        note = "use APIClient::req / get / post / ... and RequestBuilder::send"
    )]
    pub async fn request(
        &self,
        uri: &str,
        method: Method,
        headers: Headers,
        body: Option<Vec<u8>>,
        query_params: Option<&HashMap<String, String>>,
        audit: Option<AuditConfig>,
    ) -> Result<HttpResponse, APIClientError> {
        let mut builder = self.req(method, uri).headers(&headers);
        if let Some(params) = query_params {
            builder = builder.query(params);
        }
        if let Some(body) = body {
            builder = builder.body(body);
        }
        if let Some(audit) = audit {
            builder = builder.audit(audit);
        }
        builder.send().await
    }

    /// Execute an HTTP request against `base_url + uri`.
    ///
    /// Query parameters are appended to the URL; the request is bounded by the
    /// optional concurrency semaphore configured via [`Self::new`].
    ///
    /// Requests are retried under the client's [`RetryPolicy`].
    ///
    /// # Errors
    ///
    /// Returns an error if the URL cannot be parsed, the semaphore is closed,
    /// or the underlying HTTP call fails.
    #[cfg(not(feature = "audit-core"))]
    #[deprecated(
        since = "0.2.0",
        note = "use APIClient::req / get / post / ... and RequestBuilder::send"
    )]
    pub async fn request(
        &self,
        uri: &str,
        method: Method,
        headers: Headers,
        body: Option<Vec<u8>>,
        query_params: Option<&HashMap<String, String>>,
    ) -> Result<HttpResponse, APIClientError> {
        let mut builder = self.req(method, uri).headers(&headers);
        if let Some(params) = query_params {
            builder = builder.query(params);
        }
        if let Some(body) = body {
            builder = builder.body(body);
        }
        builder.send().await
    }

    /// Execute an HTTP request against `base_url + uri` with a JSON body.
    ///
    /// The body is serialized as JSON and the `Content-Type: application/json` header is set.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails or the request fails.
    #[cfg(feature = "audit-core")]
    #[deprecated(
        since = "0.2.0",
        note = "use APIClient::req / get / post / ... and RequestBuilder::send"
    )]
    #[allow(deprecated)]
    pub async fn request_json<T: Serialize + Sync>(
        &self,
        uri: &str,
        method: Method,
        headers: Headers,
        json: Option<&T>,
        query_params: Option<&HashMap<String, String>>,
        audit: Option<AuditConfig>,
    ) -> Result<HttpResponse, APIClientError> {
        let body = json
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| APIClientError::UnsupportedMethod(format!("Serialization error: {e}")))?;

        let mut headers = headers;
        if json.is_some() {
            headers = headers.content_type("application/json");
        }

        self.request(uri, method, headers, body, query_params, audit)
            .await
    }

    /// Execute an HTTP request against `base_url + uri` with a JSON body.
    ///
    /// The body is serialized as JSON and the `Content-Type: application/json` header is set.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails or the request fails.
    #[cfg(not(feature = "audit-core"))]
    #[deprecated(
        since = "0.2.0",
        note = "use APIClient::req / get / post / ... and RequestBuilder::send"
    )]
    #[allow(deprecated)]
    pub async fn request_json<T: Serialize + Sync>(
        &self,
        uri: &str,
        method: Method,
        headers: Headers,
        json: Option<&T>,
        query_params: Option<&HashMap<String, String>>,
    ) -> Result<HttpResponse, APIClientError> {
        let body = json
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| APIClientError::UnsupportedMethod(format!("Serialization error: {e}")))?;

        let mut headers = headers;
        if json.is_some() {
            headers = headers.content_type("application/json");
        }

        self.request(uri, method, headers, body, query_params).await
    }
}
