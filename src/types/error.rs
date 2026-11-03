use std::error::Error as _;

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

impl APIClientError {
    /// The error's text for logs: its message without the request URL's query
    /// (search parameters can carry personal codes), then the causes below the
    /// wrapped error, each after `": "`.
    #[must_use]
    pub fn redacted(&self) -> String {
        let mut text = self.to_string();
        if let Self::Request(e) = self
            && let Some(query) = e.url().and_then(url::Url::query)
        {
            text = text.replace(&format!("?{query}"), "");
        }
        // Every variant with a source shows it in its own message already.
        let mut cause = self.source().and_then(std::error::Error::source);
        while let Some(e) = cause {
            text.push_str(": ");
            text.push_str(&e.to_string());
            cause = e.source();
        }
        text
    }
}
