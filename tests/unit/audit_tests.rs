use crate::audit::{pretty_body, request_to_curl, shell_quote};
use crate::types::Method;
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
