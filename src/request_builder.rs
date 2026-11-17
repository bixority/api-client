#[cfg(feature = "audit-core")]
use crate::types::{AuditConfig, AuditMetadata};
use crate::types::{HttpRequest, RequestTimeout};
use crate::{APIClient, APIClientError, Headers, HttpResponse, Method};
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use std::time::Duration;

/// A request under construction, started with [`APIClient::get`],
/// [`APIClient::post`], [`APIClient::req`] and the like. Nothing is sent until
/// [`Self::send`]; a bad header or JSON value is reported there.
#[must_use = "a request is only sent by `send().await`"]
pub struct RequestBuilder<'a> {
    client: &'a APIClient,
    method: Method,
    uri: String,
    query: Vec<(String, String)>,
    headers: HeaderMap,
    body: Option<Bytes>,
    timeout: RequestTimeout,
    hold_permit: Option<bool>,
    error: Option<APIClientError>,
    #[cfg(feature = "audit-core")]
    audit: Option<AuditConfig>,
}

impl<'a> RequestBuilder<'a> {
    pub(crate) fn new(client: &'a APIClient, method: Method, uri: &str) -> Self {
        Self {
            client,
            method,
            uri: uri.to_owned(),
            query: Vec::new(),
            headers: HeaderMap::new(),
            body: None,
            timeout: RequestTimeout::Default,
            hold_permit: None,
            error: None,
            #[cfg(feature = "audit-core")]
            audit: None,
        }
    }

    /// Append query parameters. Names may repeat; the order is kept.
    pub fn query<I, K, V>(mut self, pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.query.extend(
            pairs
                .into_iter()
                .map(|(k, v)| (k.as_ref().to_owned(), v.as_ref().to_owned())),
        );
        self
    }

    /// Set a header, replacing an earlier one of the same name. An invalid name
    /// or value makes [`Self::send`] fail with [`APIClientError::InvalidHeader`].
    pub fn header(mut self, name: &str, value: &str) -> Self {
        match (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            (Ok(n), Ok(v)) => {
                self.headers.insert(n, v);
            }
            (Err(_), _) => {
                self.fail(APIClientError::InvalidHeader(format!(
                    "invalid header name `{name}`"
                )));
            }
            // The value is not echoed: it may be a credential.
            (_, Err(_)) => {
                self.fail(APIClientError::InvalidHeader(format!(
                    "invalid value for header `{name}`"
                )));
            }
        }
        self
    }

    /// Merge `headers` in; a name set here already is replaced.
    pub fn headers(mut self, headers: &Headers) -> Self {
        self.headers.extend(headers.as_map().clone());
        self
    }

    /// Send `value` serialized as JSON, with `Content-Type: application/json`.
    /// A serialization error makes [`Self::send`] fail with [`APIClientError::Json`].
    pub fn json<T: Serialize + ?Sized>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => {
                self.body = Some(Bytes::from(body));
                self.headers.insert(
                    reqwest::header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
            }
            Err(e) => self.fail(APIClientError::Json(e)),
        }
        self
    }

    /// Send `body` as the request body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Limit this request's total time, from sending to the end of the body,
    /// instead of the client's `timeout_secs`.
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = RequestTimeout::After(timeout);
        self
    }

    /// Put no time limit on this request.
    pub const fn no_timeout(mut self) -> Self {
        self.timeout = RequestTimeout::None;
        self
    }

    /// Hold the concurrency permit until the response body has been read to its
    /// end (or the response is dropped), instead of releasing it at the
    /// response head.
    ///
    /// Deadlock hazard: a task that holds `max_concurrent` responses with unread
    /// bodies and then awaits another request on the same client waits for
    /// itself. Read or drop a response before starting the next request.
    pub const fn hold_permit_until_body_end(mut self) -> Self {
        self.hold_permit = Some(true);
        self
    }

    /// Audit this request.
    #[cfg(feature = "audit-core")]
    pub fn audit(mut self, config: AuditConfig) -> Self {
        self.audit = Some(config);
        self
    }

    /// Send the request, retrying under the client's
    /// [`RetryPolicy`](crate::RetryPolicy).
    ///
    /// # Errors
    ///
    /// Returns an error if a builder step was invalid, the URL cannot be
    /// parsed, the permit wait ran out, or the HTTP call fails.
    pub async fn send(self) -> Result<HttpResponse, APIClientError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let url = self.client.build_url(&self.uri, &self.query)?;

        #[cfg(feature = "audit-core")]
        let audit_meta = if self.client.inner.has_auditor {
            self.audit.as_ref().and_then(|a| {
                a.name
                    .as_ref()
                    .map(|name| AuditMetadata::new(name, &self.uri).with_root_path(a.path.clone()))
            })
        } else {
            None
        };

        let req = HttpRequest {
            method: self.method,
            url,
            headers: self.headers,
            body: self.body,
            timeout: self.timeout,
            hold_permit: self
                .hold_permit
                .unwrap_or(self.client.inner.hold_permit_default),
            #[cfg(feature = "audit-core")]
            audit: self.audit,
            #[cfg(feature = "audit-core")]
            audit_meta,
        };
        self.client.execute(req, &self.uri).await
    }

    /// Keep the first error.
    fn fail(&mut self, e: APIClientError) {
        self.error.get_or_insert(e);
    }
}
