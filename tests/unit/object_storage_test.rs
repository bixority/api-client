use crate::audit::ObjectStorageAuditor;

#[tokio::test]
async fn test_object_storage_auditor_path() {
    let client = object_storage_client::ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(client, "base");
    assert_eq!(auditor.full_path("path"), "base/path");
    assert_eq!(auditor.full_path("/path"), "base/path");
}

#[tokio::test]
async fn test_object_storage_auditor_write() {
    use crate::audit::Auditor;
    let client = object_storage_client::ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(client, "base");
    // This should not panic and should return Ok(()) even if it fails to connect
    let result = auditor.write_audit_data("test.txt", b"data").await;
    assert!(result.is_ok());
}
