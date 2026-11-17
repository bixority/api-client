use crate::audit::{pretty_body, request_to_curl, request_to_curl_chunks, shell_quote};
use crate::types::Method;
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

#[test]
fn test_shell_quote() {
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("safe"), "safe");
    assert_eq!(shell_quote("safe_with-123"), "safe_with-123");
    assert_eq!(shell_quote("needs quote"), "'needs quote'");
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
    assert_eq!(shell_quote("complex chars: $?*"), "'complex chars: $?*'");
}

#[test]
fn test_pretty_body_json() {
    let json = r#"{"foo":"bar","baz":123}"#;
    let pretty = pretty_body(json.as_bytes());
    // Should be pretty printed JSON
    assert_eq!(pretty, "{\n  \"baz\": 123,\n  \"foo\": \"bar\"\n}");
}

#[test]
fn test_pretty_body_non_json() {
    let text = "Hello world";
    let pretty = pretty_body(text.as_bytes());
    assert_eq!(pretty, text);
}

#[test]
fn test_request_to_curl() {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("value"),
    );

    let curl = request_to_curl(
        "https://example.com/api",
        Method::Post,
        &headers,
        Some(b"body data"),
        Some("session=123"),
    );

    assert_eq!(
        curl,
        "curl -i -X POST https://example.com/api -H 'x-test: value' -b session=123 -d 'body data'"
    );
}

#[test]
fn credentials_are_redacted_in_the_curl_line() {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer eyJhbGciOiJSUzI1NiJ9.secret"),
    );
    headers.insert(
        reqwest::header::PROXY_AUTHORIZATION,
        HeaderValue::from_static("Basic dXNlcjpwYXNz"),
    );
    let mut marked = HeaderValue::from_static("api-key-123");
    marked.set_sensitive(true);
    headers.insert(HeaderName::from_static("x-api-key"), marked);
    headers.insert(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("value"),
    );

    let curl = request_to_curl("https://example.com/api", Method::Get, &headers, None, None);

    assert_eq!(
        curl,
        "curl -i -X GET https://example.com/api -H 'authorization: Bearer <redacted>' \
         -H 'proxy-authorization: Basic <redacted>' -H 'x-api-key: <redacted>' \
         -H 'x-test: value'"
    );
}

#[test]
fn audit_path_has_no_empty_segment_for_root_uri() {
    use crate::types::AuditMetadata;
    // A request to `/` has an empty URI path; that must not leave `//` in the key.
    let meta = AuditMetadata::new("nvd_saml", "/");
    let path = meta.path(Method::Post, "request");
    assert!(!path.contains("//"), "{path}");
    assert!(path.contains("/nvd_saml/POST_"), "{path}");
}

#[test]
fn audit_path_keeps_nested_uri_segments() {
    use crate::types::AuditMetadata;
    let meta = AuditMetadata::new("nvd_saml", "/v1/items/");
    let path = meta.path(Method::Get, "response");
    assert!(path.contains("/nvd_saml/v1/items/GET_"), "{path}");
}

#[test]
fn curl_body_with_single_quotes_is_escaped() {
    let curl = request_to_curl(
        "https://example.com/a b",
        Method::Post,
        &HeaderMap::new(),
        Some(b"{\"k\":\"it's\"}"),
        None,
    );
    // Hand-computed from the original implementation.
    assert_eq!(
        curl,
        "curl -i -X POST 'https://example.com/a b' -d '{\"k\":\"it'\\''s\"}'"
    );
}

#[test]
fn curl_non_utf8_body_is_a_note_in_the_log_line() {
    let curl = request_to_curl(
        "https://example.com",
        Method::Put,
        &HeaderMap::new(),
        Some(&[b'a', 0xff, b'b']),
        None,
    );
    assert_eq!(
        curl,
        "curl -i -X PUT https://example.com -d '<3 bytes, not UTF-8>'"
    );
}

fn joined(chunks: &[Bytes]) -> Vec<u8> {
    chunks.iter().flat_map(|c| c.iter().copied()).collect()
}

#[test]
fn curl_chunks_concatenate_to_request_to_curl() {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer eyJhbGciOiJSUzI1NiJ9.secret"),
    );
    headers.insert(
        reqwest::header::PROXY_AUTHORIZATION,
        HeaderValue::from_static("Basic dXNlcjpwYXNz"),
    );
    let mut marked = HeaderValue::from_static("api-key-123");
    marked.set_sensitive(true);
    headers.insert(HeaderName::from_static("x-api-key"), marked);
    headers.insert(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("it's"),
    );

    let big = format!("{{\"k\":\"{}'x\"}}", "a".repeat(1 << 20)).into_bytes();
    let bodies: Vec<Option<Vec<u8>>> = vec![
        None,
        Some(vec![]),
        Some(b"safe-only_body.1/2:3=4@5,6+7".to_vec()),
        Some(br#"{"a":1,"b":[true,null]}"#.to_vec()),
        Some(b"'start".to_vec()),
        Some(b"mid'dle".to_vec()),
        Some(b"end'".to_vec()),
        Some(b"two''in a row".to_vec()),
        Some(b"'".to_vec()),
        Some(b"'''".to_vec()),
        Some(
            "za\u{17c}\u{f3}\u{142}\u{107} 'g\u{119}\u{15b}l\u{105}' \u{1F600}"
                .as_bytes()
                .to_vec(),
        ),
        Some(vec![b'a', 0xff, b'b']),
        Some(vec![b'a', 0xff, b'\'', b'b']),
        Some(big),
    ];
    for hdrs in [&HeaderMap::new(), &headers] {
        for cookies in [None, Some("session=1; x=it's")] {
            for body in &bodies {
                let bytes = body.clone().map(Bytes::from);
                // The stored line keeps a non-UTF-8 body, lossily converted;
                // the log line shows its size instead.
                let lossy = body
                    .as_ref()
                    .map(|b| String::from_utf8_lossy(b).into_owned());
                let expected = request_to_curl(
                    "https://example.com/a b",
                    Method::Post,
                    hdrs,
                    lossy.as_deref().map(str::as_bytes),
                    cookies,
                );
                let chunks = request_to_curl_chunks(
                    "https://example.com/a b",
                    Method::Post,
                    hdrs,
                    bytes.as_ref(),
                    cookies,
                );
                assert!(
                    joined(&chunks) == expected.as_bytes(),
                    "mismatch for body of {:?} bytes",
                    body.as_ref().map(Vec::len)
                );
            }
        }
    }
}

#[test]
fn curl_chunks_share_the_body_memory() {
    let body = Bytes::from(format!(r#"{{"a":"{}","c":[1,2,3]}}"#, "b".repeat(100_000)));
    let chunks = request_to_curl_chunks(
        "https://example.com",
        Method::Post,
        &HeaderMap::new(),
        Some(&body),
        None,
    );
    // Head, opening quote, the body, closing quote.
    assert_eq!(chunks.len(), 4, "{chunks:?}");
    assert_eq!(chunks[2].as_ptr(), body.as_ptr());
    assert_eq!(chunks[2].len(), body.len());
}

#[test]
fn curl_chunks_many_quotes_stay_few_pieces() {
    let body = Bytes::from("it's a 'test' ".repeat(50_000));
    let chunks = request_to_curl_chunks(
        "https://example.com",
        Method::Post,
        &HeaderMap::new(),
        Some(&body),
        None,
    );
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    let joined: Vec<u8> = chunks.iter().flat_map(|c| c.to_vec()).collect();
    let expected = request_to_curl(
        "https://example.com",
        Method::Post,
        &HeaderMap::new(),
        Some(body.as_ref()),
        None,
    );
    assert_eq!(joined, expected.into_bytes());
}

#[test]
fn curl_chunks_safe_body_is_one_shared_piece() {
    let body = Bytes::from_static(b"safe-body");
    let chunks = request_to_curl_chunks(
        "https://example.com",
        Method::Post,
        &HeaderMap::new(),
        Some(&body),
        None,
    );
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    assert_eq!(chunks[1].as_ptr(), body.as_ptr());
}

/// The old rendering of a body that is not valid UTF-8: one lossy `String`.
fn lossy_reference(body: &[u8]) -> String {
    request_to_curl(
        "https://example.com",
        Method::Post,
        &HeaderMap::new(),
        Some(String::from_utf8_lossy(body).as_bytes()),
        None,
    )
}

#[test]
fn curl_chunks_of_binary_bodies_match_the_lossy_reference() {
    const WINDOW: usize = 64 * 1024;
    let mut bodies: Vec<Vec<u8>> = vec![
        vec![0xff],
        vec![b'a', 0xff, b'b'],
        vec![0xff, b'\'', 0xfe, b'\'', 0xc0],
        vec![0xe2, 0x82], // a truncated euro sign
        "z\u{142}'\u{20ac}".bytes().chain([0xf0, 0x9f]).collect(),
    ];
    // Invalid bytes and quotes on and around every window edge.
    for edge in [WINDOW - 4, WINDOW - 3, WINDOW - 1, WINDOW, WINDOW + 1] {
        for fill in [&b"x"[..], &b"'"[..], "\u{20ac}".as_bytes()] {
            let mut body: Vec<u8> = fill.iter().copied().cycle().take(edge).collect();
            body.extend_from_slice(&[0xff, b'\'', 0xfe]);
            body.extend(fill.iter().copied().cycle().take(100));
            bodies.push(body);
        }
    }
    // Several MiB, with long valid spans (shared with the body) between
    // invalid bytes and a quote-dense stretch.
    let mut big = Vec::new();
    for i in 0..6 {
        big.extend(std::iter::repeat_n(b'a', 700_000 + i));
        big.push(0xff);
        big.extend("it's 'q' ".repeat(20_000).bytes());
        big.push(0xc3);
    }
    bodies.push(big);

    for body in bodies {
        assert!(std::str::from_utf8(&body).is_err());
        let bytes = Bytes::from(body.clone());
        let chunks = request_to_curl_chunks(
            "https://example.com",
            Method::Post,
            &HeaderMap::new(),
            Some(&bytes),
            None,
        );
        assert!(
            joined(&chunks) == lossy_reference(&body).as_bytes(),
            "mismatch for a body of {} bytes",
            body.len()
        );
    }
}

#[test]
fn curl_chunks_of_a_binary_body_are_windows() {
    let mut body = vec![0xffu8];
    body.extend(std::iter::repeat_n(b'\'', 200_000));
    let bytes = Bytes::from(body);
    let chunks = request_to_curl_chunks(
        "https://example.com",
        Method::Post,
        &HeaderMap::new(),
        Some(&bytes),
        None,
    );
    // Quoted body 800 KB: far more than the head and a few pieces, none above a window.
    assert!(chunks.len() > 10, "{}", chunks.len());
    assert!(chunks.iter().skip(1).all(|c| c.len() <= 64 * 1024));
}
