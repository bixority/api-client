use crate::cookies::CookieJar;
use crate::types::{HttpRequest, Method};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, PROXY_AUTHORIZATION};
use url::Url;

/// What a credential is replaced with in an audited request.
pub const REDACTED: &str = "<redacted>";

/// Whether a header's value is a credential: `Authorization`,
/// `Proxy-Authorization`, or any value marked sensitive.
fn is_credential(name: &HeaderName, value: &HeaderValue) -> bool {
    value.is_sensitive() || name == AUTHORIZATION || name == PROXY_AUTHORIZATION
}

/// POSIX-shell-quote a string.
#[must_use]
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

/// Append `parts` (concatenated) to `out`, POSIX-shell-quoted exactly as
/// [`shell_quote`] would quote the concatenation.
fn push_quoted(out: &mut String, parts: &[&str]) {
    if parts.iter().all(|p| p.is_empty()) {
        out.push_str("''");
        return;
    }
    let safe = parts.iter().all(|p| {
        p.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '@' | ',' | '+')
        })
    });
    if safe {
        for p in parts {
            out.push_str(p);
        }
        return;
    }
    out.push('\'');
    for p in parts {
        for (i, chunk) in p.split('\'').enumerate() {
            if i > 0 {
                out.push_str(r"'\''");
            }
            out.push_str(chunk);
        }
    }
    out.push('\'');
}

/// Build a `curl` command string that reproduces the given request, with the
/// credentials of `Authorization`, `Proxy-Authorization` and any sensitive
/// header replaced by [`REDACTED`].
///
/// The command is written into a single pre-sized `String`; the body is
/// quoted straight into it without intermediate copies.
#[must_use]
pub fn request_to_curl(
    url: &str,
    method: Method,
    headers: &HeaderMap,
    body: Option<&[u8]>,
    cookies: Option<&str>,
) -> String {
    let method = method.to_string();
    let mut cap = "curl -i -X ".len() + method.len() + 1 + url.len() + 2;
    for (name, value) in headers {
        cap += " -H ".len() + name.as_str().len() + 2 + value.len() + 2 + REDACTED.len();
    }
    if let Some(c) = cookies {
        cap += " -b ".len() + c.len() + 2;
    }
    if let Some(b) = body {
        // Quotes plus room for a few escaped single quotes.
        cap += " -d ".len() + b.len() + 16;
    }

    let mut out = String::with_capacity(cap);
    out.push_str("curl -i -X ");
    out.push_str(&method);
    out.push(' ');
    push_quoted(&mut out, &[url]);

    for (name, value) in headers {
        let value_str = value.to_str().unwrap_or("");
        out.push_str(" -H ");
        if is_credential(name, value) {
            match value_str.split_once(' ') {
                Some((scheme, _)) if !scheme.is_empty() => {
                    push_quoted(&mut out, &[name.as_str(), ": ", scheme, " ", REDACTED]);
                }
                _ => push_quoted(&mut out, &[name.as_str(), ": ", REDACTED]),
            }
        } else {
            push_quoted(&mut out, &[name.as_str(), ": ", value_str]);
        }
    }

    if let Some(c) = cookies {
        out.push_str(" -b ");
        push_quoted(&mut out, &[c]);
    }

    if let Some(b) = body {
        out.push_str(" -d ");
        push_quoted(&mut out, &[&String::from_utf8_lossy(b)]);
    }

    out
}

/// Render a response body for auditing: pretty-printed when it is valid JSON
/// (so FHIR `OperationOutcome`/`Bundle` job results are readable), otherwise the
/// raw bytes as a lossy UTF-8 string.
#[must_use]
pub fn pretty_body(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned())
}

pub(crate) fn build_curl(req: &HttpRequest, cookies: Option<&str>) -> String {
    request_to_curl(
        &req.url,
        req.method,
        &req.headers,
        req.body.as_deref(),
        cookies,
    )
}

pub(crate) fn cookie_header_for(jar: &CookieJar, url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let header = jar.cookie_header(&parsed)?;
    header.to_str().ok().map(str::to_owned)
}
