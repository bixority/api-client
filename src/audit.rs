use crate::cookies::CookieJar;
use crate::types::{APIClientError, HttpRequest, Method};
use futures::future::BoxFuture;
use reqwest::header::HeaderMap;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};
use url::Url;

/// POSIX-shell-quote a string.
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
}

impl AuditLayer {
    pub(crate) const fn new(cookies: Arc<CookieJar>) -> Self {
        Self { cookies }
    }
}

/// Middleware service wrapper.
#[derive(Clone)]
pub struct Audit<S> {
    inner: S,
    cookies: Arc<CookieJar>,
}

impl<S> Layer<S> for AuditLayer {
    type Service = Audit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Audit {
            inner,
            cookies: self.cookies.clone(),
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
        let cookie_header = cookie_header_for(&self.cookies, &req.url);
        let curl = build_curl(&req, cookie_header.as_deref());

        Box::pin(async move {
            tracing::info!("[audit] request: {curl}");
            match inner.call(req).await {
                Ok(resp) => {
                    let status = resp.status();
                    let version = resp.version();
                    let headers = resp.headers().clone();
                    let body = resp.bytes().await?;

                    tracing::info!("[audit] response: {status}\n{}", pretty_body(&body));

                    let mut builder = http::Response::builder().status(status).version(version);
                    if let Some(h) = builder.headers_mut() {
                        *h = headers;
                    }
                    let http_resp = builder
                        .body(body)?;
                    Ok(reqwest::Response::from(http_resp))
                }
                Err(err) => {
                    tracing::error!("[audit] error response: {err:?}");
                    Err(err)
                }
            }
        })
    }
}
