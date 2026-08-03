mod audit;
mod cookies;
mod req;
mod services;
mod types;
#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod unit_tests;

pub use crate::types::{APIClientError, AuditConfig, Headers, HttpResponse, Method, StatusCode};

use crate::audit::AuditLayer;
use crate::cookies::CookieJar;
use crate::req::ReqwestService;
use crate::services::audit::Auditor;
use crate::types::HttpRequest;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::util::BoxCloneSyncService;
use tower::{Service, ServiceBuilder};
use tracing::{debug, info};
use url::Url;

#[derive(Clone)]
pub struct APIClient {
    pub base_url: String,
    timeout_secs: u64,
    max_concurrent: Option<usize>,
    audit: bool,
    auditor: Option<Arc<dyn Auditor>>,
    state: Arc<Mutex<Option<ClientInner>>>,
}

#[derive(Clone)]
struct ClientInner {
    service: BoxCloneSyncService<HttpRequest, reqwest::Response, APIClientError>,
    semaphore: Option<Arc<Semaphore>>,
    cookies: Arc<CookieJar>,
}

impl APIClient {
    /// Create a new client with default settings:
    /// - `timeout_secs`: 60
    /// - `max_concurrent`: 10
    /// - `audit`: false
    #[must_use]
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            timeout_secs: 60,
            max_concurrent: Some(10),
            audit: false,
            auditor: None,
            state: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the request timeout in seconds.
    #[must_use]
    pub fn timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self.state = Arc::new(Mutex::new(None));
        self
    }

    /// Set the maximum number of concurrent requests.
    /// Use `None` to disable concurrency limiting.
    #[must_use]
    pub fn max_concurrent(mut self, max_concurrent: Option<usize>) -> Self {
        self.max_concurrent = max_concurrent;
        self.state = Arc::new(Mutex::new(None));
        self
    }

    /// Toggle audit logging.
    #[must_use]
    pub fn with_audit(mut self, audit: bool) -> Self {
        self.audit = audit;
        self.state = Arc::new(Mutex::new(None));
        self
    }

    /// Set the auditor for request logging.
    #[must_use]
    pub fn with_auditor(mut self, auditor: Arc<dyn Auditor>) -> Self {
        debug!("API client auditor is set");
        self.auditor = Some(auditor);
        self.state = Arc::new(Mutex::new(None));
        self
    }

    /// Force initialization of the underlying HTTP client.
    ///
    /// This is called automatically on the first request, but can be used to
    /// catch configuration errors early.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client fails to initialize.
    pub fn build(self) -> Result<Self, APIClientError> {
        self.get_or_init_inner()?;
        Ok(self)
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

    async fn audit_request(
        &self,
        meta: &crate::audit::AuditMetadata<'_>,
        method: Method,
        url: &str,
        _headers: &Headers,
        body: Option<&[u8]>,
    ) -> Result<(), APIClientError> {
        let Some(auditor) = &self.auditor else {
            return Ok(());
        };

        let mut curl = format!("curl -X {method} '{url}'");
        if let Some(b) = body {
            let json_str = String::from_utf8_lossy(b);
            let _ = write!(curl, " -H 'Content-Type: application/json' -d '{json_str}'");
        }

        let audit_path = meta.path(method, "request");
        let _ = auditor.write_audit_data(&audit_path, curl.as_bytes()).await;
        Ok(())
    }

    fn get_or_init_inner(&self) -> Result<ClientInner, APIClientError> {
        {
            let guard = self
                .state
                .lock()
                .map_err(|_| APIClientError::ConcurrencyClosed)?;
            if let Some(inner) = &*guard {
                return Ok(inner.clone());
            }
        }

        let timeout = Duration::from_secs(self.timeout_secs);
        let cookies = Arc::new(CookieJar::new());
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .cookie_provider(cookies.clone())
            .build()?;

        let base = ReqwestService::new(client);
        let service = if self.audit {
            let svc = ServiceBuilder::new()
                .layer(AuditLayer::new(cookies.clone()))
                .service(base);
            BoxCloneSyncService::new(svc)
        } else {
            BoxCloneSyncService::new(base)
        };

        let inner = ClientInner {
            service,
            semaphore: self.max_concurrent.map(|n| Arc::new(Semaphore::new(n))),
            cookies,
        };

        let mut guard = self
            .state
            .lock()
            .map_err(|_| APIClientError::ConcurrencyClosed)?;
        if let Some(existing) = &*guard {
            return Ok(existing.clone());
        }

        *guard = Some(inner.clone());
        drop(guard);
        Ok(inner)
    }

    /// Drop every cookie currently held by the underlying HTTP client.
    pub fn clear_cookies(&self) {
        if let Some(inner) = self
            .state
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
        {
            inner.cookies.clear();
        }
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

    /// Execute an HTTP request against `base_url + uri` with a JSON body.
    ///
    /// The body is serialized as JSON and the `Content-Type: application/json` header is set.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails or the request fails.
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
    pub async fn request(
        &self,
        uri: &str,
        method: Method,
        headers: Headers,
        body: Option<Vec<u8>>,
        query_params: Option<&HashMap<String, String>>,
        audit: Option<AuditConfig>,
    ) -> Result<HttpResponse, APIClientError> {
        let audit_meta = audit.as_ref().and_then(|a| {
            a.name
                .as_ref()
                .map(|name| crate::audit::AuditMetadata::new(name, uri))
        });

        if let Some(ref meta) = audit_meta {
            let url = self.build_url(uri, query_params)?;
            self.audit_request(meta, method, &url, &headers, body.as_deref())
                .await?;
        }

        let mut attempts = 0;
        loop {
            attempts += 1;

            let res = self
                .request_once(
                    uri,
                    method,
                    headers.clone(),
                    body.clone(),
                    query_params,
                    audit.clone(),
                )
                .await;

            match res {
                Ok(response)
                    if attempts < Self::MAX_ATTEMPTS
                        && method == Method::Get
                        && Self::is_status_retryable(response.status()) =>
                {
                    info!(
                        "Retryable status {} for {} {}, retrying in {}ms...",
                        response.status(),
                        method,
                        uri,
                        100 * attempts
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(u64::from(100 * attempts)))
                        .await;
                }
                Ok(response) => {
                    return Ok(response);
                }
                Err(e)
                    if attempts < Self::MAX_ATTEMPTS
                        && method == Method::Get
                        && Self::is_error_retryable(&e) =>
                {
                    info!(
                        "Retryable error {} for {} {}, retrying in {}ms...",
                        e,
                        method,
                        uri,
                        100 * attempts
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(u64::from(100 * attempts)))
                        .await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn request_once(
        &self,
        uri: &str,
        method: Method,
        headers: Headers,
        body: Option<Vec<u8>>,
        query_params: Option<&HashMap<String, String>>,
        audit: Option<AuditConfig>,
    ) -> Result<HttpResponse, APIClientError> {
        let inner = self.get_or_init_inner()?;
        let url = self.build_url(uri, query_params)?;

        let req = HttpRequest {
            method,
            url,
            headers: headers.into_inner(),
            body,
            audit,
        };

        let _permit = match inner.semaphore {
            Some(sem) => Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| APIClientError::ConcurrencyClosed)?,
            ),
            None => None,
        };

        let mut svc = inner.service.clone();
        let resp = svc.call(req).await?;
        Ok(HttpResponse::from_reqwest(resp))
    }
}
