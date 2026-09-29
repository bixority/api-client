use crate::audit::{Auditor, ObjectStorageAuditor};
use crate::{APIClient, AuditConfig};
use std::sync::Arc;

#[tokio::test]
async fn test_object_storage_auditor_default() {
    let client = object_storage_client::ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(client);
    let path = auditor.full_path("path");
    if let Some(ref config_path) = AuditConfig::default().path {
        let expected = format!("{}/path", config_path.trim_matches('/'));
        assert_eq!(path, expected);
    } else {
        assert_eq!(path, "path");
    }
}

#[tokio::test]
async fn test_object_storage_auditor_path_override() {
    let client = object_storage_client::ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(client).with_audit_path("base");
    assert_eq!(auditor.full_path("path"), "base/path");
    assert_eq!(auditor.full_path("/path"), "base/path");

    let auditor_slashed =
        ObjectStorageAuditor::new(object_storage_client::ObjectStorageClient::new())
            .with_audit_path("/nested/base/");
    assert_eq!(auditor_slashed.full_path("path"), "nested/base/path");
    assert_eq!(auditor_slashed.full_path("/path"), "nested/base/path");
}

#[tokio::test]
async fn test_object_storage_auditor_write() {
    let client = object_storage_client::ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(client).with_audit_path("base");
    // This should not panic and should return Ok(()) even if it fails to connect
    let result = auditor.write_audit_data("test.txt", b"data").await;
    assert!(result.is_ok());
}

#[test]
fn test_object_storage_auditor_usage_pattern() -> Result<(), crate::APIClientError> {
    let audit_base_path = "audit_base_path".to_string();
    let sl_portal_base_url = "https://example.com";

    let osc = object_storage_client::ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(osc).with_audit_path(audit_base_path.as_str());
    let _api_client = APIClient::new(sl_portal_base_url)
        .with_auditor(Arc::new(auditor))
        .max_concurrent(Some(8))
        .timeout_secs(10)
        .build()?;

    Ok(())
}
