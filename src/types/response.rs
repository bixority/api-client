use crate::types::{APIClientError, StatusCode};
use bytes::Bytes;
use futures::Stream;
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;

/// Response wrapper that keeps `reqwest` an implementation detail.
pub struct HttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    inner: reqwest::Response,
}

impl HttpResponse {
    pub(crate) fn from_reqwest(inner: reqwest::Response) -> Self {
        let status = StatusCode(inner.status().as_u16());
        let headers = inner.headers().clone();
        Self {
            status,
            headers,
            inner,
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
        self.inner.json::<T>().await.map_err(APIClientError::from)
    }

    /// Consume the response, returning the body as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns an error if the body cannot be read, the response was already consumed,
    /// or is not valid UTF-8.
    pub async fn text(self) -> Result<String, APIClientError> {
        self.inner.text().await.map_err(APIClientError::from)
    }

    /// Consume the response, returning the body as a stream of bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the response was already consumed.
    pub fn bytes_stream(
        self,
    ) -> Result<impl Stream<Item = reqwest::Result<Bytes>>, APIClientError> {
        Ok(self.inner.bytes_stream())
    }
}
