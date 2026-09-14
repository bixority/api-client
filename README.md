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

### Object Storage Auditor

When the `audit` feature is enabled, the client provides `ObjectStorageAuditor` to store audit logs directly in S3 or S3-compatible object storage (such as MinIO).

```rust
use api_client::APIClient;
use api_client::audit::ObjectStorageAuditor;
use object_storage_client::ObjectStorageClient;
use std::sync::Arc;

let storage_client = ObjectStorageClient::new();
let auditor = ObjectStorageAuditor::new(storage_client, "s3://my-audit-bucket/audits");

let client = APIClient::new("https://api.example.com".to_string())
    .with_auditor(Arc::new(auditor))
    .build()?;
```

#### Environment Variables

Connection settings and credentials for object storage are loaded from environment variables:

- `S3_ENDPOINT` (or `AWS_ENDPOINT_URL_S3`, `AWS_ENDPOINT`, `AWS_ENDPOINT_URL`): Custom endpoint URL for S3-compatible storage.
- `S3_ACCESS_KEY_ID` (or `AWS_ACCESS_KEY_ID`): Access key ID.
- `S3_SECRET_ACCESS_KEY` (or `AWS_SECRET_ACCESS_KEY`): Secret access key.
- `S3_REGION` (or `AWS_REGION`): Storage region (defaults to `us-east-1` if not specified).
- `S3_ALLOW_HTTP`: Set to `true` to allow unencrypted HTTP connections (useful for local development). Plain HTTP is also automatically permitted if the endpoint starts with `http://`.

#### MinIO Example

To store audit logs in a local MinIO instance (for example, running via Docker at `http://localhost:9000`):

1. Set the environment variables:

```bash
export S3_ENDPOINT="http://localhost:9000"
export S3_ACCESS_KEY_ID="minioadmin"
export S3_SECRET_ACCESS_KEY="minioadmin"
export S3_REGION="us-east-1"
export S3_ALLOW_HTTP=true
```

*(Standard AWS variable names like `AWS_ENDPOINT_URL_S3`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and `AWS_REGION` are also supported.)*

2. Configure `ObjectStorageAuditor` with your MinIO bucket and base path:

```rust
use api_client::APIClient;
use api_client::audit::ObjectStorageAuditor;
use object_storage_client::ObjectStorageClient;
use std::sync::Arc;

let storage_client = ObjectStorageClient::new();
let auditor = ObjectStorageAuditor::new(storage_client, "s3://audit-logs/api-client");

let client = APIClient::new("https://api.example.com".to_string())
    .with_auditor(Arc::new(auditor))
    .build()?;
```

When requests are executed with an `AuditConfig` naming the entry (e.g. `Some(AuditConfig::new("user-request"))`), audit files will be uploaded to your MinIO bucket following the structured path format:
`s3://audit-logs/api-client/YYYY/MM/DD/{audit_name}/{uri_path}/{METHOD}_{YYMMDD_HHMMSS}_{request_id}_{request|response}.txt`

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
`YYYY/MM/DD/{audit_name}/{uri_path}/{METHOD}_{YYMMDD_HHMMSS}_{request_id}_{request|response}.txt`

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
