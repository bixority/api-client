# api-client

A high-performance, Tower-backed HTTP API client for Rust.

## Features

- **Tower Stack**: Leverages the Tower ecosystem for middleware (load balancing, retrying, rate limiting, etc.).
- **Audit Logging**: Built-in audit layer that logs requests as `curl` commands and pretty-prints JSON responses.
- **Custom Auditors**: Support for custom auditing backends (e.g., Object Storage, custom databases).
- **Concurrency Control**: Optional semaphore-based concurrency limiting.
- **Cookie Support**: Automatic cookie management.
- **Retry Logic**: Automatic retries for idempotent requests on transient errors.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
api-client = { git = "https://github.com/bixority/api-client" }
```

To enable audit logging functionality and object storage integration:

```toml
[dependencies]
api-client = { git = "https://github.com/bixority/api-client", features = ["audit"] }
```

## Usage

```rust
use api_client::{APIClient, Method, Headers};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = APIClient::new("https://api.example.com".to_string())
        .timeout_secs(5)
        .max_concurrent(Some(10))
        .build()?;

    let headers = Headers::new()
        .content_type("application/json")
        .authorization_bearer("your-token");

    let response = client.request(
        "/v1/resource",
        Method::Get,
        headers,
        None, // body
        None, // query params
    ).await?;

    if response.status().is_success() {
        let text = response.text().await?;
        println!("Response: {}", text);
    }

    Ok(())
}
```

## Audit Logging

> **Note:** Audit logging requires the `audit` feature flag to be enabled.

The client includes a powerful auditing layer. By default, if no custom auditor is provided, it logs requests and responses to the standard output using the `tracing` crate.

### Custom Auditor

You can implement the `Auditor` trait to send audit data to a custom backend. See [examples/audit.rs](examples/audit.rs) for a complete working example.

```rust
use api_client::{Auditor, APIClient, APIClientError};
use futures::future::{BoxFuture, FutureExt};
use std::sync::Arc;

struct MyAuditor;

impl Auditor for MyAuditor {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let data = data.to_vec();
        let path = path.to_string();
        async move {
            println!("Writing audit data to {}: {} bytes", path, data.len());
            // In a real implementation, you would write to a database or object storage
            Ok(())
        }
        .boxed()
    }
}

// Enable the custom auditor in the client
let client = APIClient::new(base_url)
    .with_auditor(Arc::new(MyAuditor))
    .build()?;
```

### Per-Request Configuration

Audit logging can be configured per request using `AuditConfig`. This allows you to name the audit entry or mute the response body (crucial for large responses or streaming).

```rust
use api_client::AuditConfig;

// Name the audit entry and disable response body logging
let audit = AuditConfig::new("my-request").mute_response();

let response = client.request(
    "/v1/resource",
    Method::Get,
    headers,
    None,
    None,
    Some(audit)
).await?;
```

When an auditor is used, the client generates structured paths for audit files:
`YYYY/MM/DD/{audit_name}/{uri_path}/{METHOD}_{YYMMDD_HHMMSS_ffffff}_{request_id}_{request|response}.txt`

## Streaming

To stream a response body, you MUST mute the response audit using `AuditConfig::mute_response()`. This prevents the client from buffering the entire body to log it.

```rust
use api_client::{APIClient, Method, Headers, AuditConfig};
use futures::StreamExt;

let audit = AuditConfig::new("large-download").mute_response();
let response = client.request(
    "/download",
    Method::Get,
    Headers::new(),
    None,
    None,
    Some(audit)
).await?;

let mut stream = response.bytes_stream()?;
while let Some(chunk_result) = stream.next().await {
    let chunk = chunk_result?;
    println!("Received {} bytes", chunk.len());
}
```

## License

GPL-3.0-only
