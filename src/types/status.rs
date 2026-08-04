/// Numeric HTTP status code returned by an [`HttpResponse`](crate::types::HttpResponse).
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
