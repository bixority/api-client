#![allow(clippy::significant_drop_tightening)]

use api_client::{APIClient, APIClientError, AuditConfig, Auditor};
use futures::future::{BoxFuture, FutureExt};
use std::sync::Arc;

/// A simple auditor that prints audit data to the console.
struct ConsoleAuditor;

impl Auditor for ConsoleAuditor {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let path = path.to_string();
        let data_len = data.len();
        let content = String::from_utf8_lossy(data).into_owned();

        async move {
            println!("--- AUDIT LOG START ---");
            println!("Path: {path}");
            println!("Size: {data_len} bytes");
            println!("Content:\n{content}");
            println!("--- AUDIT LOG END ---\n");
            Ok(())
        }
        .boxed()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize the client with a custom auditor
    let base_url = "https://httpbin.org".to_string();
    let client = APIClient::new(base_url)
        .with_auditor(Arc::new(ConsoleAuditor))
        .build()?;

    println!("Sending audited request to httpbin.org...\n");

    // 2. Define per-request audit configuration
    let audit_config = AuditConfig::new("example-audit");

    // 3. Execute the request
    // This will trigger the ConsoleAuditor twice: once for the request and once for the response.
    let response = client
        .post("/post")
        .header("content-type", "application/json")
        .body(r#"{"message": "Hello, Audit!"}"#)
        .audit(audit_config)
        .send()
        .await?;

    println!("Request finished with status: {}", response.status());

    // 4. Muted response body example (useful for streaming or large payloads)
    println!("\nSending another request with muted response body audit...");
    let muted_audit = AuditConfig::new("muted-audit").mute_response();

    let _ = client.get("/get").audit(muted_audit).send().await?;

    Ok(())
}
