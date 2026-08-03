# api-client

A high-performance, Tower-backed HTTP API client for Rust.

## Features

- **Tower Stack**: Leverages the Tower ecosystem for middleware (load balancing, retrying, rate limiting, etc.).
- **Audit Logging**: Built-in audit layer that logs requests as `curl` commands and pretty-prints JSON responses.
- **Concurrency Control**: Optional semaphore-based concurrency limiting.
- **Cookie Support**: Automatic cookie management.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
api-client = { git = "https://github.com/bixority/api-client" }
```

## Usage

```rust
use api_client::{APIClient, Method, Headers, AuditConfig};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = APIClient::new("https://api.example.com".to_string())
        .timeout_secs(5)
        .max_concurrent(Some(10))
        .with_audit(true)
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
        None, // audit config
    ).await?;

    if response.status().is_success() {
        let text = response.text().await?;
        println!("Response: {}", text);
    }

    Ok(())
}
```

## Audit Logging

The client uses the `tracing` crate for logging. Requests are logged as `curl` commands, and responses are pretty-printed if they contain JSON.

Audit logging can be toggled on or off via the builder:

```rust
let client = APIClient::new(base_url).with_audit(true).build()?;  // Audit enabled
let client = APIClient::new(base_url).with_audit(false).build()?; // Audit disabled
```

You can also provide a per-request `AuditConfig` to name the audit entry or to mute the response body (essential for streaming):

```rust
use api_client::AuditConfig;

// Name the audit entry and disable response body logging
let audit = AuditConfig::new("my-request").mute_response();

let response = client.request(
    uri,
    method,
    headers,
    body,
    params,
    Some(audit)
).await?;
```

Example log output with a named audit:
```text
INFO [audit:my-request] request: curl -i -X GET 'https://api.example.com/v1/resource' ...
INFO [audit:my-request] response: 200 OK (body muted)
```

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

let mut stream = response.bytes_stream().await?;
while let Some(chunk_result) = stream.next().await {
    let chunk = chunk_result?;
    println!("Received {} bytes", chunk.len());
}
```

## License

GPL-3.0-only
