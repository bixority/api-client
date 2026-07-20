use crate::{Headers, Method, StatusCode};
use std::str::FromStr;

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
