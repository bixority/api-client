mod audit;
mod cookies;
mod req;
mod types;
#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod unit_tests;

#[cfg(feature = "python")]
use pyo3::prelude::*;

pub use crate::types::{APIClientError, Headers, HttpResponse, Method, StatusCode};

use crate::audit::AuditLayer;
use crate::cookies::CookieJar;
use crate::req::ReqwestService;
use crate::types::HttpRequest;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::util::BoxCloneSyncService;
use tower::{Service, ServiceBuilder};
use url::Url;

#[cfg_attr(feature = "python", pyclass(from_py_object))]
#[derive(Clone)]
pub struct APIClient {
    pub base_url: String,
    service: BoxCloneSyncService<HttpRequest, reqwest::Response, APIClientError>,
    semaphore: Option<Arc<Semaphore>>,
    cookies: Arc<CookieJar>,
}

impl APIClient {
    /// Build a Tower-backed HTTP client.
    ///
    /// `timeout_secs` is applied per-request by the underlying HTTP backend.
    /// `max_concurrent`, when provided, caps the number of in-flight requests
    /// via an internal semaphore.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP backend fails to initialize.
    pub fn new(
        base_url: String,
        timeout_secs: u64,
        max_concurrent: Option<usize>,
    ) -> Result<Self, APIClientError> {
        let timeout = Duration::from_secs(timeout_secs);
        let cookies = Arc::new(CookieJar::new());
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .cookie_provider(cookies.clone())
            .build()?;

        let base = ReqwestService::new(client);
        let svc = ServiceBuilder::new()
            .layer(AuditLayer::new(cookies.clone()))
            .service(base);

        Ok(Self {
            base_url,
            service: BoxCloneSyncService::new(svc),
            semaphore: max_concurrent.map(|n| Arc::new(Semaphore::new(n))),
            cookies,
        })
    }

    /// Drop every cookie currently held by the underlying HTTP client.
    pub fn clear_cookies(&self) {
        self.cookies.clear();
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

    /// Execute an HTTP request against `base_url + uri`.
    ///
    /// Query parameters are appended to the URL; the request is bounded by the
    /// optional concurrency semaphore configured in [`Self::new`].
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
    ) -> Result<HttpResponse, APIClientError> {
        let url = self.build_url(uri, query_params)?;

        let req = HttpRequest {
            method,
            url,
            headers: headers.into_inner(),
            body,
        };

        let _permit = match &self.semaphore {
            Some(sem) => Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| APIClientError::ConcurrencyClosed)?,
            ),
            None => None,
        };

        let mut svc = self.service.clone();
        let resp = svc.call(req).await?;
        Ok(HttpResponse::from_reqwest(resp))
    }
}

#[cfg_attr(feature = "python", pymethods)]
impl APIClient {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (base_url, timeout_secs=30, max_concurrent=None))]
    /// Create a new `APIClient` from Python.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP backend fails to initialize.
    pub fn py_new(
        base_url: String,
        timeout_secs: u64,
        max_concurrent: Option<usize>,
    ) -> Result<Self, APIClientError> {
        Self::new(base_url, timeout_secs, max_concurrent)
    }

    #[cfg(feature = "python")]
    #[pyo3(name = "clear_cookies")]
    pub fn py_clear_cookies(&self) {
        self.clear_cookies();
    }

    #[cfg(feature = "python")]
    #[pyo3(name = "request", signature = (uri, method=Method::Get, headers=None, body=None, query_params=None))]
    fn request_py<'py>(
        &self,
        py: Python<'py>,
        uri: String,
        method: Method,
        headers: Option<Headers>,
        body: Option<Vec<u8>>,
        query_params: Option<HashMap<String, String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        use pyo3_async_runtimes::tokio::future_into_py;
        let self_clone = self.clone();
        future_into_py(py, async move {
            let resp = self_clone
                .request(
                    &uri,
                    method,
                    headers.unwrap_or_default(),
                    body,
                    query_params.as_ref(),
                )
                .await?;
            Ok(resp)
        })
    }
}

#[cfg(feature = "python")]
#[pymodule]
fn api_client(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<APIClient>()?;
    m.add_class::<Method>()?;
    m.add_class::<Headers>()?;
    m.add_class::<HttpResponse>()?;
    m.add_class::<StatusCode>()?;
    Ok(())
}
