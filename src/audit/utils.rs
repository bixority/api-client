use crate::cookies::CookieJar;
use crate::types::{HttpRequest, Method};
use bytes::Bytes;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, PROXY_AUTHORIZATION};
use url::Url;

/// What a credential is replaced with in an audited request.
pub const REDACTED: &str = "<redacted>";

/// Whether a header's value is a credential: `Authorization`,
/// `Proxy-Authorization`, or any value marked sensitive.
fn is_credential(name: &HeaderName, value: &HeaderValue) -> bool {
    value.is_sensitive() || name == AUTHORIZATION || name == PROXY_AUTHORIZATION
}

/// Whether `c` can appear in a shell word without quoting.
const fn is_safe_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '@' | ',' | '+')
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

/// Write everything of the curl command that precedes the body (`curl -i -X
/// <method> <url> -H ... [-b ...]`) into a `String` with room for `extra` more
/// bytes.
fn curl_head(
    url: &str,
    method: Method,
    headers: &HeaderMap,
    cookies: Option<&str>,
    extra: usize,
) -> String {
    let method = method.to_string();
    let mut cap = "curl -i -X ".len() + method.len() + 1 + url.len() + 2 + extra;
    for (name, value) in headers {
        cap += " -H ".len() + name.as_str().len() + 2 + value.len() + 2 + REDACTED.len();
    }
    if let Some(c) = cookies {
        cap += " -b ".len() + c.len() + 2;
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

    out
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
    // Quotes plus room for a few escaped single quotes.
    let extra = body.map_or(0, |b| " -d ".len() + b.len() + 16);
    let mut out = curl_head(url, method, headers, cookies, extra);
    if let Some(b) = body {
        out.push_str(" -d ");
        push_quoted(&mut out, &[&String::from_utf8_lossy(b)]);
    }
    out
}

/// Body spans at least this long are audited as slices of the body: the
/// object storage uploader copies shorter chunks into its own blocks.
const ZERO_COPY_MIN: usize = 64 * 1024;
/// A single quote inside a single-quoted shell word.
const QUOTE_ESCAPE: &[u8] = br"'\''";

/// Call `span(range, quoted)` for each span of `text` between single
/// quotes, in order; `quoted` is whether a single quote follows it.
fn for_each_span(text: &str, mut span: impl FnMut(std::ops::Range<usize>, bool)) {
    let mut start = 0;
    for (i, _) in text.match_indices('\'') {
        span(start..i, true);
        start = i + 1;
    }
    span(start..text.len(), false);
}

/// Same command as [`request_to_curl`], as pieces whose concatenation is
/// byte-identical to `request_to_curl(..).into_bytes()`.
///
/// Everything before the body is one owned piece. A valid UTF-8 body is not
/// copied: it is returned as zero-copy slices of `body` (cut at each single
/// quote, with the `'\''` escape as a piece in between). A body that is not
/// valid UTF-8 is quoted from its lossy conversion, as [`request_to_curl`]
/// does, which copies it.
#[must_use]
pub fn request_to_curl_chunks(
    url: &str,
    method: Method,
    headers: &HeaderMap,
    body: Option<&Bytes>,
    cookies: Option<&str>,
) -> Vec<Bytes> {
    let mut head = curl_head(url, method, headers, cookies, " -d ".len());
    let Some(body) = body else {
        return vec![Bytes::from(head)];
    };
    head.push_str(" -d ");
    let mut chunks = vec![Bytes::from(head)];

    let Ok(text) = std::str::from_utf8(body) else {
        let lossy = String::from_utf8_lossy(body);
        let mut quoted = String::with_capacity(lossy.len() + 16);
        push_quoted(&mut quoted, &[&lossy]);
        chunks.push(Bytes::from(quoted));
        return chunks;
    };

    if text.is_empty() {
        chunks.push(Bytes::from_static(b"''"));
    } else if text.chars().all(is_safe_char) {
        chunks.push(body.clone());
    } else {
        // Spans of at least ZERO_COPY_MIN bytes are shared with the body.
        // The shorter spans and the escapes between two shared spans go
        // into one owned buffer, so a body full of quotes does not become a
        // list of tiny pieces. A first pass sizes each buffer exactly: a
        // `Bytes` made from a `Vec` keeps all of its capacity.
        let mut sizes = Vec::new();
        let mut size = 1; // the opening quote
        for_each_span(text, |span, quoted| {
            if span.len() >= ZERO_COPY_MIN {
                sizes.push(size);
                size = 0;
            } else {
                size += span.len();
            }
            if quoted {
                size += QUOTE_ESCAPE.len();
            }
        });
        sizes.push(size + 1); // the closing quote

        let mut sizes = sizes.into_iter();
        let mut buf = Vec::with_capacity(sizes.next().unwrap_or(0));
        buf.push(b'\'');
        for_each_span(text, |span, quoted| {
            if span.len() >= ZERO_COPY_MIN {
                let next = Vec::with_capacity(sizes.next().unwrap_or(0));
                chunks.push(Bytes::from(std::mem::replace(&mut buf, next)));
                chunks.push(body.slice(span));
            } else {
                buf.extend_from_slice(&body[span]);
            }
            if quoted {
                buf.extend_from_slice(QUOTE_ESCAPE);
            }
        });
        buf.push(b'\'');
        chunks.push(Bytes::from(buf));
    }
    chunks
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
