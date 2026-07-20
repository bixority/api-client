use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use std::time::Duration;

/// HTTP method recognized by [`APIClient`](super::APIClient).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
}

impl Method {
    pub(crate) const fn into_reqwest(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Delete => reqwest::Method::DELETE,
            Self::Patch => reqwest::Method::PATCH,
            Self::Head => reqwest::Method::HEAD,
        }
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
        })
    }
}

impl std::str::FromStr for Method {
    type Err = APIClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "get" => Ok(Self::Get),
            "post" => Ok(Self::Post),
            "put" => Ok(Self::Put),
            "delete" => Ok(Self::Delete),
            "patch" => Ok(Self::Patch),
            "head" => Ok(Self::Head),
            other => Err(APIClientError::UnsupportedMethod(other.to_string())),
        }
    }
}

/// Numeric HTTP status code returned by an [`HttpResponse`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusCode(pub u16);

impl StatusCode {
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.0)
    }

    #[must_use]
    pub fn is_client_error(&self) -> bool {
        (400..500).contains(&self.0)
    }

    #[must_use]
    pub fn is_server_error(&self) -> bool {
        (500..600).contains(&self.0)
    }
}

impl std::fmt::Display for StatusCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Builder for the request header collection.
#[derive(Clone, Debug, Default)]
pub struct Headers {
    inner: HeaderMap,
}

impl Headers {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HeaderMap::new(),
        }
    }

    #[must_use]
    pub fn insert(mut self, key: &str, value: &str) -> Self {
        if let (Ok(name), Ok(val)) = (HeaderName::try_from(key), HeaderValue::try_from(value)) {
            self.inner.insert(name, val);
        }
        self
    }

    #[must_use]
    pub fn content_type(self, content_type: &str) -> Self {
        self.insert("content-type", content_type)
    }

    #[must_use]
    pub fn authorization_bearer(self, token: &str) -> Self {
        self.insert("authorization", &format!("Bearer {token}"))
    }

    pub(crate) fn into_inner(self) -> HeaderMap {
        self.inner
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        self.inner
            .get(key)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }
}

/// Internal request representation passed through the Tower service stack.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Option<Vec<u8>>,
}

/// Response wrapper that keeps `reqwest` an implementation detail.
pub struct HttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    inner: std::sync::Arc<tokio::sync::Mutex<Option<reqwest::Response>>>,
}

impl HttpResponse {
    pub(crate) fn from_reqwest(inner: reqwest::Response) -> Self {
        let status = StatusCode(inner.status().as_u16());
        let headers = inner.headers().clone();
        Self {
            status,
            headers,
            inner: std::sync::Arc::new(tokio::sync::Mutex::new(Some(inner))),
        }
    }

    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Get a single header value as a UTF-8 string (lossy headers are dropped).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }

    /// Consume the response, parsing the body as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if the body cannot be read, the response was already consumed,
    /// or fails to deserialize into `T`.
    pub async fn json<T: DeserializeOwned>(self) -> Result<T, APIClientError> {
        let mut guard = self.inner.lock().await;
        let resp = guard.take().ok_or(APIClientError::ConcurrencyClosed)?;
        drop(guard);
        resp.json::<T>().await.map_err(APIClientError::from)
    }

    /// Consume the response, returning the body as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns an error if the body cannot be read, the response was already consumed,
    /// or is not valid UTF-8.
    pub async fn text(self) -> Result<String, APIClientError> {
        let mut guard = self.inner.lock().await;
        let resp = guard.take().ok_or(APIClientError::ConcurrencyClosed)?;
        drop(guard);
        resp.text().await.map_err(APIClientError::from)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum APIClientError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),

    #[error("HTTP error {0}")]
    Http(StatusCode),

    #[error("rate limited, retry_after={0:?}")]
    RateLimited(Option<Duration>),

    #[error("circuit open")]
    CircuitOpen,

    #[error("concurrency limiter closed")]
    ConcurrencyClosed,

    #[error("unsupported HTTP method: {0}")]
    UnsupportedMethod(String),

    #[error("internal HTTP error: {0}")]
    InternalHttp(#[from] http::Error),
}
