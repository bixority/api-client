#[cfg(feature = "audit")]
pub mod audit;
pub mod builder;
mod cookies;
mod req;
pub mod types;

#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod unit_tests;

#[cfg(feature = "audit")]
pub use crate::audit::{AuditLayer, Auditor};
pub use crate::builder::APIClientBuilder;
pub use crate::types::{APIClientError, Headers, HttpResponse, Method, StatusCode};
#[cfg(feature = "audit")]
pub use crate::types::{AuditConfig, AuditMetadata};

use crate::cookies::CookieJar;
use crate::types::HttpRequest;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::Service;
use tower::util::BoxCloneSyncService;
use tracing::info;
use url::Url;

#[derive(Clone)]
pub struct APIClient {
    pub base_url: String,
    pub(crate) inner: Arc<ClientInner>,
}

#[derive(Clone)]
pub(crate) struct ClientInner {
    pub(crate) service: BoxCloneSyncService<HttpRequest, reqwest::Response, APIClientError>,
    pub(crate) semaphore: Option<Arc<Semaphore>>,
    pub(crate) cookies: Arc<CookieJar>,
}

impl APIClient {
    /// Create a new [`APIClientBuilder`] with default settings.
    #[allow(clippy::new_ret_no_self)]
    #[must_use]
    pub const fn new(base_url: String) -> APIClientBuilder {
        APIClientBuilder::new(base_url)
    }

    /// Create a new [`APIClientBuilder`] with default settings.
    #[must_use]
    pub const fn builder(base_url: String) -> APIClientBuilder {
        APIClientBuilder::new(base_url)
    }

    const MAX_ATTEMPTS: u32 = 3;

    fn is_error_retryable(e: &APIClientError) -> bool {
        match e {
            APIClientError::Request(re) => re.is_connect() || re.is_timeout() || re.is_request(),
            _ => false,
        }
    }

    const fn is_status_retryable(status: StatusCode) -> bool {
        let code = status.as_u16();
        code == 502 || code == 503 || code == 504
    }

    /// Drop every cookie currently held by the underlying HTTP client.
    pub fn clear_cookies(&self) {
        self.inner.cookies.clear();
    }

    fn build_url(
        &self,
        uri: &str,
        query_params: Option<&HashMap<String, String>>,
    ) -> Result<String, APIClientError> {
        let base = self.base_url.trim_end_matches('/');
        let path = uri.trim_start_matches('/');
        let url = format!("{base}/{path}");

        let Some(params) = query_params else {
            return Ok(url);
        };

        let mut parsed = Url::parse(&url)?;
        {
            let mut pairs = parsed.query_pairs_mut();
            for (k, v) in params {
                pairs.append_pair(k, v);
            }
        }
        Ok(parsed.into())
    }

    async fn request_once(&self, req: HttpRequest) -> Result<HttpResponse, APIClientError> {
        let _permit = match self.inner.semaphore.as_ref() {
            Some(sem) => Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| APIClientError::ConcurrencyClosed)?,
            ),
            None => None,
        };

        let mut svc = self.inner.service.clone();
        let resp = svc.call(req).await?;
        Ok(HttpResponse::from_reqwest(resp))
    }

    /// Execute an HTTP request against `base_url + uri`.
    ///
    /// Query parameters are appended to the URL; the request is bounded by the
    /// optional concurrency semaphore configured via [`Self::new`].
    ///
    /// Requests are automatically retried up to 3 times for `GET` requests on
    /// transient network errors or 5xx server errors (502, 503, 504).
    ///
    /// # Errors
    ///
    /// Returns an error if the URL cannot be parsed, the semaphore is closed,
    /// or the underlying HTTP call fails.
    #[cfg(feature = "audit")]
    pub async fn request(
        &self,
        uri: &str,
        method: Method,
        headers: Headers,
        body: Option<Vec<u8>>,
        query_params: Option<&HashMap<String, String>>,
        audit: Option<AuditConfig>,
    ) -> Result<HttpResponse, APIClientError> {
        let url = self.build_url(uri, query_params)?;
        let audit_meta = audit
            .as_ref()
            .and_then(|a| a.name.as_ref().map(|name| AuditMetadata::new(name, uri)));

        let req = HttpRequest {
            method,
            url,
            headers: headers.into_inner(),
            body,
            audit,
            audit_meta,
        };

        let mut attempts = 0;
        loop {
            attempts += 1;

            match self.request_once(req.clone()).await {
                Ok(response) if Self::should_retry_status(&response, method, attempts) => {
                    self.wait_for_retry(method, uri, response.status().as_u16(), attempts)
                        .await;
                }
                Ok(response) => return Ok(response),
                Err(e) if Self::should_retry_error(&e, method, attempts) => {
                    self.wait_for_retry(method, uri, &e, attempts).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Execute an HTTP request against `base_url + uri`.
    ///
    /// Query parameters are appended to the URL; the request is bounded by the
    /// optional concurrency semaphore configured via [`Self::new`].
    ///
    /// Requests are automatically retried up to 3 times for `GET` requests on
    /// transient network errors or 5xx server errors (502, 503, 504).
    ///
    /// # Errors
    ///
    /// Returns an error if the URL cannot be parsed, the semaphore is closed,
    /// or the underlying HTTP call fails.
    #[cfg(not(feature = "audit"))]
    pub async fn request(
        &self,
        uri: &str,
        method: Method,
        headers: Headers,
        body: Option<Vec<u8>>,
        query_params: Option<&HashMap<String, String>>,
    ) -> Result<HttpResponse, APIClientError> {
        let url = self.build_url(uri, query_params)?;

        let req = HttpRequest {
            method,
            url,
            headers: headers.into_inner(),
            body,
        };

        let mut attempts = 0;
        loop {
            attempts += 1;

            match self.request_once(req.clone()).await {
                Ok(response) if Self::should_retry_status(&response, method, attempts) => {
                    self.wait_for_retry(method, uri, response.status().as_u16(), attempts)
                        .await;
                }
                Ok(response) => return Ok(response),
                Err(e) if Self::should_retry_error(&e, method, attempts) => {
                    self.wait_for_retry(method, uri, &e, attempts).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn should_retry_status(resp: &HttpResponse, method: Method, attempts: u32) -> bool {
        attempts < Self::MAX_ATTEMPTS
            && method == Method::Get
            && Self::is_status_retryable(resp.status())
    }

    fn should_retry_error(err: &APIClientError, method: Method, attempts: u32) -> bool {
        attempts < Self::MAX_ATTEMPTS && method == Method::Get && Self::is_error_retryable(err)
    }

    async fn wait_for_retry(
        &self,
        method: Method,
        uri: &str,
        reason: impl std::fmt::Display,
        attempts: u32,
    ) {
        let delay = 100 * attempts;
        info!(
            "Retryable {} {} for {} {}, retrying in {}ms...",
            if attempts == 1 { "condition" } else { "error" },
            reason,
            method,
            uri,
            delay
        );
        tokio::time::sleep(Duration::from_millis(u64::from(delay))).await;
    }

    /// Execute an HTTP request against `base_url + uri` with a JSON body.
    ///
    /// The body is serialized as JSON and the `Content-Type: application/json` header is set.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails or the request fails.
    #[cfg(feature = "audit")]
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
    #[cfg(not(feature = "audit"))]
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
