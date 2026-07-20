use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;
use std::sync::{Arc, RwLock};
use url::Url;

/// Cookie store shared between the `reqwest` client and the audit layer.
///
/// Wraps [`reqwest::cookie::Jar`] in an [`RwLock`] so the inner jar can be
/// replaced when callers ask to clear all cookies.
#[derive(Default)]
pub struct CookieJar {
    inner: RwLock<Arc<Jar>>,
}

impl CookieJar {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Drop every stored cookie.
    pub(crate) fn clear(&self) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Arc::new(Jar::default());
    }

    /// Return the `Cookie` header value that would be sent for `url`, if any.
    pub(crate) fn cookie_header(&self, url: &Url) -> Option<HeaderValue> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.cookies(url)
    }
}

impl CookieStore for CookieJar {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.set_cookies(cookie_headers, url);
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        self.cookie_header(url)
    }
}
