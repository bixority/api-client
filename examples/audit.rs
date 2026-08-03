use api_client::{APIClient, APIClientError, AuditConfig, Auditor, Headers, Method};
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

    // 2. Prepare headers
    let headers = Headers::new().content_type("application/json");

    // 3. Define per-request audit configuration
    let audit_config = AuditConfig::new("example-audit");

    // 4. Execute the request
    // This will trigger the ConsoleAuditor twice: once for the request and once for the response.
    let response = client
        .request(
            "/post",
            Method::Post,
            headers,
            Some(b"{\"message\": \"Hello, Audit!\"}".to_vec()),
            None,
            Some(audit_config),
        )
        .await?;

    println!("Request finished with status: {}", response.status());

    // 5. Muted response body example (useful for streaming or large payloads)
    println!("\nSending another request with muted response body audit...");
    let muted_audit = AuditConfig::new("muted-audit").mute_response();

    let _ = client
        .request(
            "/get",
            Method::Get,
            Headers::new(),
            None,
            None,
            Some(muted_audit),
        )
        .await?;

    Ok(())
}
