use crate::types::{APIClientError, StatusCode};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use tokio::sync::OwnedSemaphorePermit;

/// Response wrapper that keeps `reqwest` an implementation detail.
pub struct HttpResponse {
    status: StatusCode,
    inner: reqwest::Response,
    /// The concurrency permit, when it is held until the body ends.
    permit: Option<OwnedSemaphorePermit>,
}

impl HttpResponse {
    pub(crate) fn from_reqwest(
        inner: reqwest::Response,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Self {
        let status = StatusCode(inner.status().as_u16());
        Self {
            status,
            inner,
            permit,
        }
    }

    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Get a single header value as a UTF-8 string (lossy headers are dropped).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<String> {
        self.inner
            .headers()
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
        let Self {
            inner,
            permit: _permit,
            ..
        } = self;
        inner.json::<T>().await.map_err(APIClientError::from)
    }

    /// Consume the response, returning the body as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns an error if the body cannot be read, the response was already consumed,
    /// or is not valid UTF-8.
    pub async fn text(self) -> Result<String, APIClientError> {
        let Self {
            inner,
            permit: _permit,
            ..
        } = self;
        inner.text().await.map_err(APIClientError::from)
    }

    /// Consume the response, returning the body as bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the body cannot be read or the response was already consumed.
    pub async fn bytes(self) -> Result<Bytes, APIClientError> {
        let Self {
            inner,
            permit: _permit,
            ..
        } = self;
        inner.bytes().await.map_err(APIClientError::from)
    }

    /// Consume the response, returning the body as a stream of bytes.
    ///
    /// A permit held until the body ends (see
    /// [`APIClientBuilder::hold_permit_until_body_end`](crate::APIClientBuilder::hold_permit_until_body_end))
    /// is released when the stream yields `None`, or when it is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the response was already consumed.
    pub fn bytes_stream(
        self,
    ) -> Result<impl Stream<Item = reqwest::Result<Bytes>>, APIClientError> {
        let Self { inner, permit, .. } = self;
        // The permit moves into a stream that ends without items once the body
        // stream is done; dropping the chain earlier drops it too.
        // (`ready` keeps the stream `Unpin`.)
        let release = futures::stream::once(std::future::ready(permit)).filter_map(|permit| {
            drop(permit);
            std::future::ready(None::<reqwest::Result<Bytes>>)
        });
        Ok(inner.bytes_stream().chain(release))
    }
}
