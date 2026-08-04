use crate::{AuditConfig, AuditMetadata, Headers, HttpResponse, Method, StatusCode};
use futures::StreamExt;
use std::str::FromStr;

#[tokio::test]
async fn test_http_response_methods() {
    let http_resp = http::Response::builder()
        .status(200)
        .header("x-test", "value")
        .body(reqwest::Body::from("{\"result\":\"ok\"}"))
        .expect("Failed to build mock response");
    let resp = reqwest::Response::from(http_resp);
    let my_resp = HttpResponse::from_reqwest(resp);

    assert_eq!(my_resp.status().as_u16(), 200);
    assert_eq!(
        my_resp.header("x-test").expect("Missing x-test header"),
        "value"
    );
    assert_eq!(
        my_resp.text().await.expect("Failed to get text"),
        "{\"result\":\"ok\"}"
    );

    // Test JSON (need to recreate response as text() consumes body)
    let http_resp = http::Response::builder()
        .status(200)
        .body(reqwest::Body::from("{\"result\":\"ok\"}"))
        .expect("Failed to build mock JSON response");
    let resp = reqwest::Response::from(http_resp);
    let my_resp = HttpResponse::from_reqwest(resp);
    let json: serde_json::Value = my_resp.json().await.expect("Failed to parse JSON");
    assert_eq!(json["result"], "ok");

    // Test bytes_stream
    let http_resp = http::Response::builder()
        .status(200)
        .body(reqwest::Body::from("data"))
        .expect("Failed to build mock stream response");
    let resp = reqwest::Response::from(http_resp);
    let my_resp = HttpResponse::from_reqwest(resp);
    let mut stream = my_resp.bytes_stream().expect("Failed to get bytes stream");
    let bytes = stream
        .next()
        .await
        .expect("Stream ended unexpectedly")
        .expect("Stream error");
    assert_eq!(bytes, "data");
}

#[test]
fn test_method_from_str() -> Result<(), crate::APIClientError> {
    assert_eq!(Method::from_str("GET")?, Method::Get);
    assert_eq!(Method::from_str("post")?, Method::Post);
    assert_eq!(Method::from_str("  PUT  ")?, Method::Put);
    assert_eq!(Method::from_str("DELETE")?, Method::Delete);
    assert_eq!(Method::from_str("patch")?, Method::Patch);
    assert_eq!(Method::from_str("HEAD")?, Method::Head);
    assert!(Method::from_str("INVALID").is_err());
    Ok(())
}

#[test]
fn test_method_display() {
    assert_eq!(Method::Get.to_string(), "GET");
    assert_eq!(Method::Post.to_string(), "POST");
}

#[test]
fn test_status_code() {
    let sc = StatusCode(200);
    assert!(sc.is_success());
    assert!(!sc.is_client_error());
    assert!(!sc.is_server_error());
    assert_eq!(sc.as_u16(), 200);

    let sc = StatusCode(404);
    assert!(!sc.is_success());
    assert!(sc.is_client_error());
    assert!(!sc.is_server_error());

    let sc = StatusCode(500);
    assert!(!sc.is_success());
    assert!(!sc.is_client_error());
    assert!(sc.is_server_error());
}

#[test]
fn test_headers_builder() {
    let headers = Headers::new()
        .insert("X-Test", "Value")
        .content_type("application/json")
        .authorization_bearer("token123");

    assert_eq!(headers.get("x-test"), Some("Value".to_string()));
    assert_eq!(
        headers.get("content-type"),
        Some("application/json".to_string())
    );
    assert_eq!(
        headers.get("authorization"),
        Some("Bearer token123".to_string())
    );
}

#[test]
fn test_audit_config() {
    let config = AuditConfig::new("test");
    assert_eq!(config.name, Some("test".to_string()));
    assert!(config.audit_response_body);

    let config = config.mute_response();
    assert!(!config.audit_response_body);
}

#[test]
fn test_audit_metadata() {
    let meta = AuditMetadata::new("test-audit", "/some/path?query=1");
    assert_eq!(meta.audit_name, "test-audit");
    assert_eq!(meta.uri_path, "some/path?query=1");
    assert!(!meta.date_path.is_empty());
    assert!(!meta.timestamp.is_empty());
    assert!(!meta.request_id.is_empty());

    let path = meta.path(Method::Get, "request");
    let expected = format!(
        "{}/{}/{}/{}_{}_{}_request.txt",
        meta.date_path,
        meta.audit_name,
        meta.uri_path,
        Method::Get,
        meta.timestamp,
        meta.request_id
    );
    assert_eq!(path, expected);
}
