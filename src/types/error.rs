#[derive(thiserror::Error, Debug)]
pub enum APIClientError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),

    #[error("concurrency limiter closed")]
    ConcurrencyClosed,

    #[error("unsupported HTTP method: {0}")]
    UnsupportedMethod(String),

    #[error("internal HTTP error: {0}")]
    InternalHttp(#[from] http::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
