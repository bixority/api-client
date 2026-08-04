use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

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
