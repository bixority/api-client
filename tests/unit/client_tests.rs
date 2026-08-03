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
