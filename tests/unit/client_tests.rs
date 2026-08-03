use crate::APIClient;
use std::collections::HashMap;

#[test]
fn test_build_url() -> Result<(), crate::APIClientError> {
    let client = APIClient::new("https://api.example.com".to_string())
        .timeout_secs(30)
        .max_concurrent(None)
        .with_audit(false)
        .build()?;

    // Test basic URL building
    let url = client.build_url("test", None)?;
    assert_eq!(url, "https://api.example.com/test");

    // Test with leading/trailing slashes
    let url = client.build_url("/test/", None)?;
    assert_eq!(url, "https://api.example.com/test/");

    // Test with query parameters
    let mut params = HashMap::new();
    params.insert("key".to_string(), "value".to_string());
    params.insert("foo".to_string(), "bar".to_string());

    let url = client.build_url("search", Some(&params))?;
    // Order of query params might not be guaranteed, so we check for both
    assert!(url.contains("key=value"));
    assert!(url.contains("foo=bar"));
    assert!(url.starts_with("https://api.example.com/search?"));

    Ok(())
}

#[test]
fn test_client_init_audit_toggle() -> Result<(), crate::APIClientError> {
    // Verify client can be initialized with audit enabled
    let client_audit = APIClient::new("https://api.example.com".to_string())
        .timeout_secs(30)
        .max_concurrent(None)
        .with_audit(true)
        .build()?;
    assert_eq!(client_audit.base_url, "https://api.example.com");

    // Verify client can be initialized with audit disabled
    let client_no_audit = APIClient::new("https://api.example.com".to_string())
        .timeout_secs(30)
        .max_concurrent(None)
        .with_audit(false)
        .build()?;
    assert_eq!(client_no_audit.base_url, "https://api.example.com");

    Ok(())
}

#[test]
fn test_client_builder_defaults() -> Result<(), crate::APIClientError> {
    let client = APIClient::new("https://api.example.com".to_string()).build()?;
    assert_eq!(client.base_url, "https://api.example.com");
    Ok(())
}

#[tokio::test]
async fn test_request_with_audit_config() -> Result<(), crate::APIClientError> {
    let _client = APIClient::new("https://api.example.com".to_string()).build()?;

    // This test just verifies that the new signature works and we can pass AuditConfig
    // We don't actually send a request because there's no mock server here,
    // but the compilation check and this call confirm the API changes.

    let audit = crate::AuditConfig::new("test-audit").mute_response();
    assert!(!audit.audit_response_body);
    assert_eq!(audit.name, Some("test-audit".to_string()));

    // We don't call request() here because it would try to connect to api.example.com

    Ok(())
}
