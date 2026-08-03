use crate::cookies::CookieJar;
use crate::types::{APIClientError, HttpRequest, Method};
use futures::FutureExt;
use futures::future::BoxFuture;
use object_storage_client::ObjectStorageClient;
use reqwest::header::HeaderMap;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};
use tracing::{error, info, warn};
use url::Url;

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

/// POSIX-shell-quote a string.
#[must_use]
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '@' | ',' | '+')
    });
    if safe {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Build a `curl` command string that reproduces the given request.
fn request_to_curl(
    url: &str,
    method: Method,
    headers: &HeaderMap,
    body: Option<&[u8]>,
    cookies: Option<&str>,
) -> String {
    let mut parts: Vec<String> = vec![
        "curl".to_string(),
        "-i".to_string(),
        "-X".to_string(),
        method.to_string(),
        shell_quote(url),
    ];

    for (name, value) in headers {
        let value_str = value.to_str().unwrap_or("");
        let header_line = format!("{}: {}", name.as_str(), value_str);
        parts.push("-H".to_string());
        parts.push(shell_quote(&header_line));
    }

    if let Some(c) = cookies {
        parts.push("-b".to_string());
        parts.push(shell_quote(c));
    }

    if let Some(b) = body {
        let body_str = String::from_utf8_lossy(b);
        parts.push("-d".to_string());
        parts.push(shell_quote(&body_str));
    }

    parts.join(" ")
}

/// Render a response body for auditing: pretty-printed when it is valid JSON
/// (so FHIR `OperationOutcome`/`Bundle` job results are readable), otherwise the
/// raw bytes as a lossy UTF-8 string.
#[must_use]
pub fn pretty_body(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned())
}

fn build_curl(req: &HttpRequest, cookies: Option<&str>) -> String {
    request_to_curl(
        &req.url,
        req.method,
        &req.headers,
        req.body.as_deref(),
        cookies,
    )
}

fn cookie_header_for(jar: &CookieJar, url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let header = jar.cookie_header(&parsed)?;
    header.to_str().ok().map(str::to_owned)
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

            if let (Some(auditor), Some(meta)) = (auditor.as_ref(), req_audit_meta.as_ref()) {
                let path = meta.path(req_method, "request");
                let _ = auditor.write_audit_data(&path, curl.as_bytes()).await;
            } else {
                info!("{log_prefix} request: {curl}");
            }

            match inner.call(req).await {
                Ok(resp) => {
                    let status = resp.status();

                    if !audit_response_body {
                        info!("{log_prefix} response: {status} (body muted)");
                        return Ok(resp);
                    }

                    let version = resp.version();
                    let headers = resp.headers().clone();
                    let body = resp.bytes().await?;
                    let pretty = pretty_body(&body);

                    if let (Some(auditor), Some(meta)) = (auditor.as_ref(), req_audit_meta.as_ref())
                    {
                        let path = meta.path(req_method, "response");
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
                Err(err) => {
                    error!("{log_prefix} error response: {err:?}");
                    Err(err)
                }
            }
        })
    }
}
