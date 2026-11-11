pub mod utils;

use bytes::Bytes;
use utils::{build_curl, cookie_header_for};
pub use utils::{pretty_body, request_to_curl, request_to_curl_chunks, shell_quote};

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

    /// Like [`Self::write_audit_data`], but takes ownership of the data so an
    /// implementation can store it without copying. The default borrows it.
    fn write_audit_owned(
        &self,
        path: &str,
        data: Vec<u8>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        self.write_audit_data(path, &data)
    }

    /// Like [`Self::write_audit_owned`], but the data comes as consecutive
    /// pieces, so a large request body can be audited without first joining
    /// it into one buffer. The stored object is the pieces concatenated.
    ///
    /// The default joins the pieces into one `Vec` and calls
    /// [`Self::write_audit_owned`]; an implementation that can stream should
    /// override it.
    fn write_audit_chunks(
        &self,
        path: &str,
        chunks: Vec<Bytes>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let total = chunks.iter().map(Bytes::len).sum();
        let mut data = Vec::with_capacity(total);
        for c in &chunks {
            data.extend_from_slice(c);
        }
        drop(chunks);
        self.write_audit_owned(path, data)
    }
}

/// Auditor that writes audit data to object storage.
#[allow(dead_code)]
#[derive(Clone)]
pub struct ObjectStorageAuditor {
    client: ObjectStorageClient,
}

impl ObjectStorageAuditor {
    /// Create a new `ObjectStorageAuditor`.
    #[must_use]
    pub const fn new(client: ObjectStorageClient) -> Self {
        Self { client }
    }
}

impl Auditor for ObjectStorageAuditor {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        self.write_audit_owned(path, data.to_vec())
    }

    fn write_audit_owned(
        &self,
        path: &str,
        data: Vec<u8>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let path = path.to_owned();
        let data_len = data.len();
        let client = self.client.clone();
        async move {
            match client.put(&path, data).await {
                Ok(()) => info!("Audit data written to {}: {} bytes", path, data_len),
                Err(e) => warn!("Failed to write audit data to {}: {}", path, e),
            }
            // We don't want to fail the request if auditing fails
            Ok(())
        }
        .boxed()
    }

    fn write_audit_chunks(
        &self,
        path: &str,
        chunks: Vec<Bytes>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let path = path.to_owned();
        let data_len: usize = chunks.iter().map(Bytes::len).sum();
        let client = self.client.clone();
        async move {
            // The http(s) backend has no multipart upload: `put_stream`
            // would buffer the object anyway and refuses one above 1 GiB,
            // which `put` stores. So it gets the joined object, as before.
            if is_http(&path) {
                let mut data = Vec::with_capacity(data_len);
                for c in chunks {
                    data.extend_from_slice(&c);
                }
                match client.put(&path, data).await {
                    Ok(()) => info!("Audit data written to {}: {} bytes", path, data_len),
                    Err(e) => warn!("Failed to write audit data to {}: {}", path, e),
                }
                return Ok(());
            }
            let stream = futures::stream::iter(chunks.into_iter().map(Ok));
            match client.put_stream(&path, stream).await {
                Ok(()) => info!("Audit data written to {}: {} bytes", path, data_len),
                Err(e) => warn!("Failed to write audit data to {}: {}", path, e),
            }
            // We don't want to fail the request if auditing fails
            Ok(())
        }
        .boxed()
    }
}

/// Whether `url` is an `http(s)://` address.
fn is_http(url: &str) -> bool {
    url.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

/// The largest response body audited pretty-printed. Above it the body is
/// audited as received, with no `Value` tree or pretty copy.
const PRETTY_MAX: usize = 256 * 1024;

/// The request as it is audited: pieces for an auditor, a curl line to log.
enum AuditRequest {
    Chunks(Vec<Bytes>),
    Line(String),
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
        let audit_request = if auditor.is_some() && req.audit_meta.is_some() {
            AuditRequest::Chunks(request_to_curl_chunks(
                &req.url,
                req.method,
                &req.headers,
                req.body.as_ref(),
                cookie_header.as_deref(),
            ))
        } else {
            AuditRequest::Line(build_curl(&req, cookie_header.as_deref()))
        };
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
                audit_request,
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
                    // Without the URL's query: search parameters can carry
                    // personal codes.
                    error!("{log_prefix} error response: {}", err.redacted());
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
        request: AuditRequest,
        log_prefix: &str,
    ) {
        match (auditor, meta, request) {
            (Some(auditor), Some(meta), AuditRequest::Chunks(chunks)) => {
                let path = meta.path(method, "request");
                let _ = auditor.write_audit_chunks(&path, chunks).await;
            }
            (Some(auditor), Some(meta), AuditRequest::Line(curl)) => {
                let path = meta.path(method, "request");
                let _ = auditor.write_audit_owned(&path, curl.into_bytes()).await;
            }
            (_, _, AuditRequest::Line(curl)) => info!("{log_prefix} request: {curl}"),
            (_, _, AuditRequest::Chunks(_)) => {}
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

        if let (Some(auditor), Some(meta)) = (auditor, meta) {
            let path = meta.path(method, "response");
            if body.len() <= PRETTY_MAX {
                let pretty = pretty_body(&body);
                let _ = auditor.write_audit_owned(&path, pretty.into_bytes()).await;
            } else {
                let _ = auditor.write_audit_chunks(&path, vec![body.clone()]).await;
            }
        } else if body.len() <= PRETTY_MAX {
            let pretty = pretty_body(&body);
            info!("{log_prefix} response: {status}\n{pretty}");
        } else {
            let text = String::from_utf8_lossy(&body);
            info!("{log_prefix} response: {status}\n{text}");
        }

        let mut builder = http::Response::builder().status(status).version(version);
        if let Some(h) = builder.headers_mut() {
            *h = headers;
        }
        let http_resp = builder.body(body)?;
        Ok(reqwest::Response::from(http_resp))
    }
}
