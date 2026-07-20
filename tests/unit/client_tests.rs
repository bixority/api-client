use crate::APIClient;
use std::collections::HashMap;

#[test]
fn test_build_url() {
    let client = APIClient::new("https://api.example.com".to_string(), 30, None).unwrap();

    // Test basic URL building
    let url = client.build_url("test", None).unwrap();
    assert_eq!(url, "https://api.example.com/test");

    // Test with leading/trailing slashes
    let url = client.build_url("/test/", None).unwrap();
    assert_eq!(url, "https://api.example.com/test/");

    // Test with query parameters
    let mut params = HashMap::new();
    params.insert("key".to_string(), "value".to_string());
    params.insert("foo".to_string(), "bar".to_string());

    let url = client.build_url("search", Some(&params)).unwrap();
    // Order of query params might not be guaranteed, so we check for both
    assert!(url.contains("key=value"));
    assert!(url.contains("foo=bar"));
    assert!(url.starts_with("https://api.example.com/search?"));
}
