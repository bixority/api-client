pub mod utils;

use utils::{build_curl, cookie_header_for};
pub use utils::{pretty_body, request_to_curl, shell_quote};

use crate::cookies::CookieJar;
use crate::types::{APIClientError, AuditMetadata, HttpRequest, Method};
use futures::FutureExt;
use futures::future::BoxFuture;
use object_storage_client::ObjectStorageClient;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};
use tracing::{error, info, warn};

pub trait Auditor: Send + Sync {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>>;
}

/// Auditor that writes audit data to object storage.
#[allow(dead_code)]
#[derive(Clone)]
pub struct ObjectStorageAuditor {
    client: ObjectStorageClient,
    base_path: String,
}

impl ObjectStorageAuditor {
    /// Create a new `ObjectStorageAuditor` with a given `base_path`.
    #[must_use]
    pub fn new(client: ObjectStorageClient, base_path: &str) -> Self {
        Self {
            client,
            base_path: base_path.trim_end_matches('/').to_owned(),
        }
    }

    /// Return the full object storage path for a given relative path.
    #[must_use]
    pub fn full_path(&self, path: &str) -> String {
        format!("{}/{}", self.base_path, path.trim_start_matches('/'))
    }
}

impl Auditor for ObjectStorageAuditor {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let full_path = self.full_path(path);
        let data = data.to_vec();
        let data_len = data.len();
        let client = self.client.clone();
        async move {
            match client.put(&full_path, data).await {
                Ok(()) => {
                    info!("Audit data written to {}: {} bytes", full_path, data_len);
                    Ok(())
                }
                Err(e) => {
                    warn!("Failed to write audit data to {}: {}", full_path, e);
                    // We don't want to fail the request if auditing fails
                    Ok(())
                }
            }
        }
        .boxed()
    }
}

/// Audit layer (middleware factory).
#[derive(Clone)]
pub struct AuditLayer {
    cookies: Arc<CookieJar>,
    auditor: Option<Arc<dyn Auditor>>,
}

impl AuditLayer {
    pub(crate) fn new(cookies: Arc<CookieJar>, auditor: Option<Arc<dyn Auditor>>) -> Self {
        Self { cookies, auditor }
    }
}

/// Middleware service wrapper.
#[derive(Clone)]
pub struct Audit<S> {
    inner: S,
    cookies: Arc<CookieJar>,
    auditor: Option<Arc<dyn Auditor>>,
}

impl<S> Layer<S> for AuditLayer {
    type Service = Audit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Audit {
            inner,
            cookies: self.cookies.clone(),
            auditor: self.auditor.clone(),
        }
    }
}

impl<S> Service<HttpRequest> for Audit<S>
where
    S: Service<HttpRequest, Response = reqwest::Response, Error = APIClientError>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    type Response = reqwest::Response;
    type Error = APIClientError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        let mut inner = self.inner.clone();
        let auditor = self.auditor.clone();
        let cookie_header = cookie_header_for(&self.cookies, &req.url);
        let curl = build_curl(&req, cookie_header.as_deref());
        let audit_response_body = req.audit.as_ref().is_none_or(|a| a.audit_response_body);
        let log_prefix = req
            .audit
            .as_ref()
            .and_then(|a| a.name.as_deref())
            .map_or_else(|| "[audit]".to_string(), |n| format!("[audit:{n}]"));

        Box::pin(async move {
            let req_method = req.method;
            let req_audit_meta = req.audit_meta.clone();

            Self::audit_request(
                auditor.as_deref(),
                req_audit_meta.as_ref(),
                req_method,
                &curl,
                &log_prefix,
            )
            .await;

            match inner.call(req).await {
                Ok(resp) => {
                    Self::audit_response(
                        auditor.as_deref(),
                        req_audit_meta.as_ref(),
                        req_method,
                        resp,
                        audit_response_body,
                        &log_prefix,
                    )
                    .await
                }
                Err(err) => {
                    error!("{log_prefix} error response: {err:?}");
                    Err(err)
                }
            }
        })
    }
}

impl<S> Audit<S> {
    async fn audit_request(
        auditor: Option<&dyn Auditor>,
        meta: Option<&AuditMetadata>,
        method: Method,
        curl: &str,
        log_prefix: &str,
    ) {
        if let (Some(auditor), Some(meta)) = (auditor, meta) {
            let path = meta.path(method, "request");
            let _ = auditor.write_audit_data(&path, curl.as_bytes()).await;
        } else {
            info!("{log_prefix} request: {curl}");
        }
    }

    async fn audit_response(
        auditor: Option<&dyn Auditor>,
        meta: Option<&AuditMetadata>,
        method: Method,
        resp: reqwest::Response,
        audit_body: bool,
        log_prefix: &str,
    ) -> Result<reqwest::Response, APIClientError> {
        let status = resp.status();

        if !audit_body {
            info!("{log_prefix} response: {status} (body muted)");
            return Ok(resp);
        }

        let version = resp.version();
        let headers = resp.headers().clone();
        let body = resp.bytes().await?;
        let pretty = pretty_body(&body);

        if let (Some(auditor), Some(meta)) = (auditor, meta) {
            let path = meta.path(method, "response");
            let _ = auditor.write_audit_data(&path, pretty.as_bytes()).await;
        } else {
            info!("{log_prefix} response: {status}\n{pretty}");
        }

        let mut builder = http::Response::builder().status(status).version(version);
        if let Some(h) = builder.headers_mut() {
            *h = headers;
        }
        let http_resp = builder.body(body)?;
        Ok(reqwest::Response::from(http_resp))
    }
}
