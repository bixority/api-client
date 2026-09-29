# api-client

A high-performance, Tower-backed HTTP API client for Rust.

## Features

- **Tower Stack**: Leverages the Tower ecosystem for middleware (load balancing, retrying, rate limiting, etc.).
- **Audit Logging**: Built-in audit layer that logs requests as `curl` commands and pretty-prints JSON responses.
- **Custom & S3 Auditors**: First-class support for storing audit logs in S3/Garage object storage or custom audit 
backends.
- **Concurrency Control**: Optional semaphore-based concurrency limiting.
- **Connection Pooling**: Configurable idle connection limits per host.
- **Cookie Support**: Automatic cookie management and session jar clearing.
- **Retry Logic**: Automatic retries for idempotent requests on transient errors and 5xx status codes.
- **Response Streaming**: Stream large response bodies with audit mute support.

## Installation

Add `api-client` to your `Cargo.toml`:

```toml
[dependencies]
api-client = { git = "https://github.com/bixority/api-client" }
```

To enable audit logging functionality and S3/Garage object storage integration, enable the `audit` feature:

```toml
[dependencies]
api-client = { git = "https://github.com/bixority/api-client", features = ["audit"] }
object-storage-client = { version = "0.1", registry = "bixority-codeberg" }
```

## Environment Variables

When using the `audit` feature with `ObjectStorageAuditor`, connection settings and S3 credentials are automatically 
loaded from environment variables:

- `API_CLIENT_AUDIT_PATH`: Root path for audit log storage (loaded into `AuditConfig`).
- `S3_ENDPOINT` (or `AWS_ENDPOINT_URL_S3`, `AWS_ENDPOINT`, `AWS_ENDPOINT_URL`): Custom endpoint URL for S3 or 
S3-compatible storage (e.g., `http://localhost:3900` for Garage).
- `S3_ACCESS_KEY_ID` (or `AWS_ACCESS_KEY_ID`): Access key ID / username.
- `S3_SECRET_ACCESS_KEY` (or `AWS_SECRET_ACCESS_KEY`): Secret access key / password.
- `S3_REGION` (or `AWS_REGION`): Storage region (defaults to `us-east-1` if not specified).
- `S3_ALLOW_HTTP`: Set to `true` to allow plain HTTP connections for local development (automatically permitted if 
endpoint begins with `http://`).

### Environment Configuration Example

```bash
export S3_ENDPOINT="http://localhost:9000"
export S3_ACCESS_KEY_ID="rootroot"
export S3_SECRET_ACCESS_KEY="rootrootrootroot"
export S3_REGION="us-east-1"
export S3_ALLOW_HTTP=true
```

## Usage

Below is a complete usage example demonstrating how to configure environment variables, initialize `APIClient` with 
`ObjectStorageAuditor`, and execute audited HTTP requests:

```rust
use api_client::audit::ObjectStorageAuditor;
use api_client::{APIClient, AuditConfig, Headers, Method};
use object_storage_client::ObjectStorageClient;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Setup environment variables (typically done via shell / .env file)
    // std::env::set_var("S3_ENDPOINT", "http://localhost:3900");
    // std::env::set_var("S3_ACCESS_KEY_ID", "rootroot");
    // std::env::set_var("S3_SECRET_ACCESS_KEY", "rootrootrootroot");
    // std::env::set_var("S3_REGION", "us-east-1");
    // std::env::set_var("S3_ALLOW_HTTP", "true");

    // 2. Initialize ObjectStorageClient and ObjectStorageAuditor
    let storage_client = ObjectStorageClient::new();
    let auditor = ObjectStorageAuditor::new(storage_client)
        .with_audit_path("s3://audit-logs/api-client");

    // 3. Build APIClient with the auditor and custom options
    let client = APIClient::new("https://httpbin.org".to_string())
        .timeout_secs(10)
        .max_concurrent(Some(10))
        .pool_max_idle_per_host(5)
        .with_auditor(Arc::new(auditor))
        .build()?;

    // 4. Construct request headers
    let headers = Headers::new()
        .content_type("application/json")
        .authorization_bearer("your-token");

    // 5. Configure audit entry name for this request
    let audit_config = AuditConfig::new("user-login");

    // 6. Execute the request
    let response = client
        .request(
            "/post",
            Method::Post,
            headers,
            Some(b"{\"username\": \"alice\"}".to_vec()),
            None, // query params
            Some(audit_config),
        )
        .await?;

    if response.status().is_success() {
        let text = response.text().await?;
        println!("Response: {}", text);
    }

    Ok(())
}
```

> **Note:** If the `audit` feature is disabled, `APIClient::request` accepts 5 parameters (`uri`, `method`, `headers`, 
> `body`, `query_params`) without the trailing `Option<AuditConfig>`.

## Audit Logging

> **Note:** Audit logging requires the `audit` feature flag to be enabled in `Cargo.toml`.

The client includes an auditing layer that captures:
1. **Request:** Reconstructs the outgoing request as an executable `curl` command.
2. **Response:** Logs response status and pretty-prints JSON bodies (or raw text).

### Default Tracing Auditor

If no custom auditor is passed via `.with_auditor(...)`, the client automatically logs audit output through the 
`tracing` crate at `INFO` level.

### Object Storage Auditor (S3 / Garage)

`ObjectStorageAuditor` uploads audit files to any S3-compatible object storage. By default, 
`ObjectStorageAuditor::new()` defaults to the `AuditConfig` path (initialized from `API_CLIENT_AUDIT_PATH`), which can 
be overridden using `.with_audit_path(...)`.

```rust
use api_client::APIClient;
use api_client::audit::ObjectStorageAuditor;
use object_storage_client::ObjectStorageClient;
use std::sync::Arc;

let storage_client = ObjectStorageClient::new();
// Default audit path from API_CLIENT_AUDIT_PATH, or override with .with_audit_path(...)
let auditor = ObjectStorageAuditor::new(storage_client)
    .with_audit_path("s3://my-audit-bucket/audits");

let client = APIClient::new("https://api.example.com")
    .with_auditor(Arc::new(auditor))
    .build()?;
```

#### Audit File Path Format

When an audit name is provided in `AuditConfig`, files are uploaded using the following structured path layout:
```text
{base_path}/[{root_path}/]YYYY/MM/DD/{audit_name}/{uri_path}/{METHOD}_{YYMMDD_HHMMSS}_{request_id}_{request|response}.txt
```

### Custom Auditor

You can implement the `Auditor` trait to route audit entries to any backend (e.g., custom database, message queue, or 
stdout console). See [examples/audit.rs](examples/audit.rs) for a complete working example.

```rust
use api_client::{APIClient, APIClientError, Auditor};
use futures::future::{BoxFuture, FutureExt};
use std::sync::Arc;

struct ConsoleAuditor;

impl Auditor for ConsoleAuditor {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let path = path.to_string();
        let content = String::from_utf8_lossy(data).into_owned();
        async move {
            println!("Audit entry [{path}]:\n{content}");
            Ok(())
        }
        .boxed()
    }
}

let client = APIClient::new("https://api.example.com".to_string())
    .with_auditor(Arc::new(ConsoleAuditor))
    .build()?;
```

### Per-Request Configuration

Audit logging is configured per-request using `AuditConfig`:

```rust
use api_client::AuditConfig;

// Name the audit entry and mute response body (e.g., for large responses or streaming)
let audit = AuditConfig::new("file-download").mute_response();

let response = client
    .request(
        "/download",
        Method::Get,
        Headers::new(),
        None,
        None,
        Some(audit),
    )
    .await?;
```

## Streaming

When streaming large response bodies, you must mute the response audit with `AuditConfig::mute_response()`. This 
prevents the client from buffering the entire response payload into memory.

```rust
use api_client::{APIClient, AuditConfig, Headers, Method};
use futures::StreamExt;

let audit = AuditConfig::new("large-download").mute_response();
let response = client
    .request(
        "/stream",
        Method::Get,
        Headers::new(),
        None,
        None,
        Some(audit),
    )
    .await?;

let mut stream = response.bytes_stream()?;
while let Some(chunk_result) = stream.next().await {
    let chunk = chunk_result?;
    println!("Received chunk: {} bytes", chunk.len());
}
```

## Cookie Management

The client manages cookies across requests automatically. To reset the session and clear stored cookies:

```rust
client.clear_cookies();
```

## License

GPL-3.0-only
