# api-client

An HTTP API client for Rust built on `reqwest` and a Tower service stack.

## Features

- **Audit Tower Layer**: An optional layer (`audit-core`) that logs requests as `curl` commands and pretty-prints
  JSON responses. Credentials (`Authorization`, `Proxy-Authorization` and any header marked sensitive) are written as
  `<redacted>`, keeping their scheme; retry and error log lines leave out the URL's query.
- **Custom & S3 Auditors**: The `Auditor` trait for your own backend, and `ObjectStorageAuditor` (`audit`) for
  S3/Garage object storage.
- **Concurrency Limit**: An optional semaphore that bounds the number of requests in flight.
- **Connection Pooling**: Configurable idle connection limits per host; by default the idle cap equals
  `max_concurrent`.
- **HTTP/2**: Negotiated through ALPN by default; `http1_only()` switches to HTTP/1.1.
- **Response Decompression**: gzip, brotli, zstd and deflate, on by default.
- **Cookie Support**: Cookie jar shared by all requests, with session clearing.
- **GET Retry**: A hand-written retry loop for `GET` requests on transient network errors and 502/503/504 statuses
  (3 attempts, 100 ms then 200 ms backoff).
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

With `audit`, `api-client` links only the S3 backend of `object-storage-client` (no CLI, encryption, GCS, Azure or HTTP
backends), so the audit path must be an `s3://` URL. For AWS profiles that use SSO or `credential_process`, enable
`audit-aws-profile` instead. If your own code needs other backends, enable them on your own `object-storage-client`
dependency; Cargo unifies the features.

### Cargo features

| Feature             | Default | Meaning                                                                                  |
|---------------------|---------|------------------------------------------------------------------------------------------|
| `gzip`              | yes     | Decompress `gzip` responses.                                                             |
| `brotli`            | yes     | Decompress `br` responses.                                                               |
| `zstd`              | yes     | Decompress `zstd` responses.                                                             |
| `deflate`           | yes     | Decompress `deflate` responses.                                                          |
| `audit-core`        | no      | The audit layer, the `Auditor` trait and `AuditConfig`; no storage backend is linked.    |
| `audit`             | no      | `audit-core` plus `ObjectStorageAuditor` (S3 only).                                      |
| `audit-aws-profile` | no      | `audit`, plus AWS profiles that use SSO or `credential_process`.                         |

Compressed responses are decoded transparently and carry no `Content-Length`. To turn decompression off, use
`default-features = false`.

### Builder options

- `timeout_secs(u64)`: total request timeout, default 60.
- `connect_timeout_secs(u64)`: connect timeout, default 10.
- `read_timeout(Duration)`: time to the first byte and the longest gap between body chunks; off by default.
- `max_concurrent(Option<usize>)`: concurrency limit, default `Some(10)`.
- `pool_max_idle_per_host(usize)`: idle connections per host; defaults to `max_concurrent` (unlimited when that is
  `None`).
- `http1_only()`: use HTTP/1.1 only. HTTP/2 is the default when the server offers it.
- `http2_connection_window(u32)`: HTTP/2 connection window in bytes. The default is `(max_concurrent + 2) * 2 MiB`,
  at most `2^31 - 1`; with `max_concurrent(None)` the HTTP/2 default is kept.

### musl builds

If you ship a musl binary that uses this crate, set a global allocator such as `mimalloc` in your binary; musl's
default allocator is slow under concurrent load. This library does not set one.

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
    let auditor = ObjectStorageAuditor::new(storage_client);

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

> **Note:** If the `audit-core` feature is disabled, `APIClient::request` accepts 5 parameters (`uri`, `method`, `headers`, 
> `body`, `query_params`) without the trailing `Option<AuditConfig>`.

## Audit Logging

> **Note:** The audit layer requires the `audit-core` feature, and `ObjectStorageAuditor` requires `audit`.

The client includes an auditing layer that captures:
1. **Request:** Reconstructs the outgoing request as an executable `curl` command.
2. **Response:** The body is stored exactly as received, at every size. On the log path (no auditor, or
   no audit name) the status and body go to `tracing` at `DEBUG`: JSON up to 256 KiB pretty-printed, other
   UTF-8 as is, and a body that is not UTF-8 as `<N bytes, not UTF-8>`.

A response body is read whole for the audit only when its head says it is small: a `Content-Length` up to
`APIClientBuilder::audit_body_limit` (default 4 MiB). A longer body, or one without a `Content-Length` (chunked, or
decoded from `gzip`, `br`, `zstd` or `deflate`, which reqwest decompresses transparently), is not buffered and
the caller gets it as a stream. Its record is then head-only: the status line, the headers (credentials and
`set-cookie` values replaced by `<redacted>`) and `[body not audited: content-length N]` or
`[body not audited: unknown length]`. With `AuditConfig::tee_response()` the whole body is stored instead, as the
caller reads it (through `Auditor::write_audit_stream`, with 8 chunks of backpressure); if the body ends early nothing
is stored under `response`, and a `response_partial` head record notes the bytes seen. Call
`APIClient::flush_audit().await` before shutdown to wait for such writes.

A request with no `AuditConfig` is not audited at all. The record of the request is written while the call is in
flight, and the concurrency permit is released before the response record is written. Records are written under a
separate limit, `APIClientBuilder::audit_write_limit` (default twice `max_concurrent`, `None` for no limit).

### Default Tracing Auditor

If no custom auditor is passed via `.with_auditor(...)`, the client automatically logs audit output through the 
`tracing` crate at `DEBUG` level.

### Object Storage Auditor (S3 / Garage)

`ObjectStorageAuditor` uploads audit files to any S3-compatible object storage. It writes each audit object under the 
key provided by `AuditConfig` / `AuditMetadata` without adding any prefix of its own. The root path comes from 
`AuditConfig` (set via `API_CLIENT_AUDIT_PATH` or `AuditConfig::with_path(...)`).

```rust
use api_client::APIClient;
use api_client::audit::ObjectStorageAuditor;
use object_storage_client::ObjectStorageClient;
use std::sync::Arc;

let storage_client = ObjectStorageClient::new();
let auditor = ObjectStorageAuditor::new(storage_client);

let client = APIClient::new("https://api.example.com")
    .with_auditor(Arc::new(auditor))
    .build()?;
```

#### Audit File Path Format

When an audit name is provided in `AuditConfig`, files are uploaded using the following structured path layout:
```text
[{root_path}/]YYYY/MM/DD/{audit_name}/{uri_path}/{METHOD}_{YYMMDD_HHMMSS}_{request_id}_{request|response}.txt
```

### Custom Auditor

You can implement the `Auditor` trait to route audit entries to any backend (e.g., custom database, message queue, or 
stdout console). The layer calls `write_audit_chunks` for every record; override it to store the pieces without
joining them. Its default passes a single piece to `write_audit_data` and joins several for `write_audit_owned`.
See [examples/audit.rs](examples/audit.rs) for a complete working example.

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

### Background writes

`BackgroundAuditor` wraps any auditor and writes in the background, so a slow store never delays a request:

```rust
use api_client::{APIClient, BackgroundAuditor, BackgroundOptions};
use std::sync::Arc;

let background = BackgroundAuditor::new(Arc::new(auditor), BackgroundOptions::default());
let client = APIClient::new("https://api.example.com")
    .with_auditor(Arc::new(background.clone()))
    .build()?;

// ... requests ...

background.flush().await; // before a clean shutdown
```

Records wait in a queue bounded by bytes (default 64 MiB); one that does not fit is dropped and counted
(`dropped()`). 16 writers run at once and each write gets 30 s (`timed_out()`); see `BackgroundOptions`. The trade-offs:
records still queued are lost if the process is killed, the request record is no longer stored before the request is
sent, and the records of retried `GET` attempts may land out of order. A teed response body
(`AuditConfig::tee_response`) is passed to the inner auditor directly.

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

A response body is buffered for the audit only when its `Content-Length` is within `audit_body_limit` (see Audit
Logging); larger or unknown-length bodies stream to you with a head-only audit record. To skip the response audit
altogether, mute it with `AuditConfig::mute_response()`.

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

## Migrating from 0.1

See [CHANGELOG.md](CHANGELOG.md) for the changes in 0.2.0: the request builder, the changed audit record format, the
INFO to DEBUG move of the audit logs, and the other breaking changes.

## License

GPL-3.0-only
