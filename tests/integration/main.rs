//! End-to-end tests of the object-storage audit against real stores.
//!
//! Each test starts its own container (Garage for S3, Azurite for Azure Blob),
//! sends a request to a local mock server and reads the audit records back
//! from the store. They need a container runtime (`DOCKER_HOST`).
//! `ObjectStorageAuditor` swallows store errors, so only reading back proves a
//! record was written.

mod harness;

use api_client::{APIClient, AuditConfig, Headers, Method};
use harness::{Store, audited_client, mock_server};
use object_storage_client::ObjectStorageClient;
use std::fmt::Write as _;
use std::time::Duration;

const AUDIT_NAME: &str = "it_audit";
const URI: &str = "/system/login";

/// Sends one audited POST to the mock server and returns the status and body.
async fn send(client: &APIClient, root: &str) -> (u16, String) {
    let response = client
        .request(
            URI,
            Method::Post,
            Headers::new().content_type("text/plain"),
            Some(b"request-payload".to_vec()),
            None,
            Some(AuditConfig::new(AUDIT_NAME).with_path(root)),
        )
        .await
        .expect("request must succeed");
    let status = response.status().as_u16();
    (status, response.text().await.expect("response body"))
}

/// Lists the records below `root` and returns the one ending in `suffix`,
/// after checking the documented path shape.
async fn find_record(store: &ObjectStorageClient, root: &str, suffix: &str) -> String {
    let keys = store
        .list(&format!("{root}/"))
        .await
        .expect("list audit records");
    let key = keys
        .iter()
        .find(|k| k.ends_with(suffix))
        .unwrap_or_else(|| panic!("no *{suffix} record in {keys:?}"));
    // {YYYY/MM/DD}/{audit_name}/{uri_path}/{METHOD}_{yymmdd_HHMMSS}_{id}_{kind}.txt
    let date = chrono::Utc::now().format("%Y/%m/%d").to_string();
    let tail = format!("{AUDIT_NAME}/system/login/POST_");
    assert!(
        key.starts_with(&format!("{date}/{tail}")),
        "unexpected key {key}"
    );
    key.clone()
}

async fn read(store: &ObjectStorageClient, url: &str) -> String {
    let bytes = store.get(url).await.expect("read audit record");
    String::from_utf8(bytes.to_vec()).expect("utf-8 record")
}

/// Runs the full scenario against `store`: request, then read both records back.
async fn audit_round_trip(store: &Store) {
    let mock = mock_server("response-body-marker").await;
    let client = audited_client(&mock.base_url, store.vars());
    let reader = ObjectStorageClient::with_vars(store.vars());
    let root = store.root();

    let (status, body) = send(&client, &root).await;
    assert_eq!(status, 200);
    assert_eq!(body, "response-body-marker", "caller still gets the body");

    let request_key = find_record(&reader, &root, "_request.txt").await;
    let response_key = find_record(&reader, &root, "_response.txt").await;
    let request = read(&reader, &format!("{root}/{request_key}")).await;
    let response = read(&reader, &format!("{root}/{response_key}")).await;

    assert!(request.starts_with("curl -i -X POST"), "{request}");
    assert!(
        request.contains(&format!("{}{URI}", mock.base_url)),
        "{request}"
    );
    assert!(request.contains("request-payload"), "{request}");
    assert_eq!(response, "response-body-marker");
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_is_written_to_s3_garage() {
    let store = Store::garage().await;
    audit_round_trip(&store).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_is_written_to_azure_azurite() {
    let store = Store::azurite().await;
    audit_round_trip(&store).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_store_does_not_fail_the_request() {
    // Nothing listens on this port: every audit write fails.
    let store = Store::unreachable_s3();
    let mock = mock_server("response-body-marker").await;
    let client = audited_client(&mock.base_url, store.vars());

    let (status, body) =
        tokio::time::timeout(Duration::from_secs(120), send(&client, &store.root()))
            .await
            .expect("request must not hang on a failing store");
    assert_eq!(status, 200);
    assert_eq!(body, "response-body-marker");
}

#[tokio::test(flavor = "multi_thread")]
async fn large_body_with_quotes_is_stored_byte_identical() {
    let store = Store::garage().await;
    let mock = mock_server("ok").await;
    let client = audited_client(&mock.base_url, store.vars());
    let reader = ObjectStorageClient::with_vars(store.vars());
    let root = store.root();

    // Above 8 MiB, so the audit is a multipart upload; with single quotes.
    let mut body = String::from("{\"items\":[");
    let mut i = 0;
    while body.len() < 9 * 1024 * 1024 {
        if i > 0 {
            body.push(',');
        }
        let _ = write!(body, "\"it's #{i} \u{142}\"");
        i += 1;
    }
    body.push_str("]}");

    // The mock answers after one read, so the upload of the body may be cut
    // short; the request audit is written before the call and is what is checked.
    let _ = client
        .request(
            URI,
            Method::Post,
            Headers::new().content_type("application/json"),
            Some(body.clone().into_bytes()),
            None,
            Some(AuditConfig::new(AUDIT_NAME).with_path(&root)),
        )
        .await;

    let key = find_record(&reader, &root, "_request.txt").await;
    let stored = reader
        .get(&format!("{root}/{key}"))
        .await
        .expect("read audit record");

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let expected = api_client::audit::request_to_curl(
        &format!("{}{URI}", mock.base_url),
        Method::Post,
        &headers,
        Some(body.as_bytes()),
        None,
    );
    assert_eq!(stored.len(), expected.len());
    assert!(
        stored.as_ref() == expected.as_bytes(),
        "stored object differs"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn large_response_is_stored_byte_identical() {
    let store = Store::garage().await;
    let reader = ObjectStorageClient::with_vars(store.vars());
    let root = store.root();

    // Above the 256 KiB pretty limit: audited as received, not re-rendered
    // (compact JSON would differ from its pretty form).
    let mut body = String::from("{\"items\":[");
    let mut i = 0;
    while body.len() < 400 * 1024 {
        if i > 0 {
            body.push(',');
        }
        let _ = write!(body, "\"it's #{i} \u{142}\"");
        i += 1;
    }
    body.push_str("]}");

    let mock = mock_server(Box::leak(body.clone().into_boxed_str())).await;
    let client = audited_client(&mock.base_url, store.vars());
    let (status, got) = send(&client, &root).await;
    assert_eq!(status, 200);
    assert_eq!(got, body, "caller still gets the body");

    let key = find_record(&reader, &root, "_response.txt").await;
    let stored = reader
        .get(&format!("{root}/{key}"))
        .await
        .expect("read audit record");
    assert_eq!(stored.len(), body.len());
    assert!(stored.as_ref() == body.as_bytes(), "stored object differs");
}
