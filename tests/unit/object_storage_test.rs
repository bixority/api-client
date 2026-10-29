use crate::APIClient;
use crate::audit::{Auditor, ObjectStorageAuditor};
use crate::types::{AuditConfig, AuditMetadata, Method};
use std::sync::Arc;

#[tokio::test]
async fn test_object_storage_auditor_write() {
    // Written below a temporary directory: a relative name would land in the
    // working directory.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("test.txt");
    let client = object_storage_client::ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(client);
    let result = auditor
        .write_audit_data(&file.display().to_string(), b"data")
        .await;
    assert!(result.is_ok());
    assert_eq!(std::fs::read(&file).expect("written"), b"data");
}

#[test]
fn test_object_storage_auditor_usage_pattern() -> Result<(), crate::APIClientError> {
    let sl_portal_base_url = "https://example.com";

    let osc = object_storage_client::ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(osc);
    let _api_client = APIClient::new(sl_portal_base_url)
        .with_auditor(Arc::new(auditor))
        .max_concurrent(Some(8))
        .timeout_secs(10)
        .build()?;

    Ok(())
}

#[test]
fn test_metadata_path_with_object_storage_auditor() {
    let audit_config = AuditConfig::new("sl_portal").with_path("s3://default/audit");
    let meta = AuditMetadata::new(
        audit_config
            .name
            .as_deref()
            .expect("audit name is configured"),
        "/system/login",
    )
    .with_root_path(audit_config.path);

    let meta_path = meta.path(Method::Post, "response");

    assert!(meta_path.starts_with("s3://default/audit/"));
    assert!(!meta_path.starts_with("s3://default/audit/s3://default/audit/"));
    assert!(meta_path.contains("/sl_portal/system/login/POST_"));
}
