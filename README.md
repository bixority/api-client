# api-client

A high-performance, Tower-backed HTTP API client for Rust and Python.

## Features

- **Tower Stack**: Leverages the Tower ecosystem for middleware (load balancing, retrying, rate limiting, etc.).
- **Audit Logging**: Built-in audit layer that logs requests as `curl` commands and pretty-prints JSON responses.
- **Concurrency Control**: Optional semaphore-based concurrency limiting.
- **Cookie Support**: Automatic cookie management.
- **Python Bindings**: First-class async support for Python using PyO3.

## Installation

### Rust

Add to your `Cargo.toml`:

```toml
[dependencies]
api-client = { git = "https://github.com/bixority/api-client" }
```

### Python

```bash
pip install api-client
```

## Usage

### Rust

```rust
use api_client::{APIClient, Method, Headers};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = APIClient::new(
        "https://api.example.com".to_string(),
        5, // timeout in seconds
        Some(10), // max concurrent requests
    )?;

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

### Python

```python
import asyncio
from api_client import APIClient, Method, Headers

async def main():
    client = APIClient(
        "https://api.example.com",
        timeout_secs=5,
        max_concurrent=10
    )

    headers = Headers()
    headers.content_type("application/json")
    headers.authorization_bearer("your-token")

    response = await client.request(
        "/v1/resource",
        Method.Get,
        headers=headers
    )

    if response.status.is_success():
        data = await response.json()
        print(f"Data: {data}")

if __name__ == "__main__":
    asyncio.run(main())
```

## Audit Logging

The client uses the `tracing` crate for logging. Requests are logged as `curl` commands, and responses are pretty-printed if they contain JSON.

Example log output:
```text
INFO [audit] request: curl -i -X GET 'https://api.example.com/v1/resource' -H 'content-type: application/json'
INFO [audit] response: 200 OK
{
  "status": "success",
  "data": { ... }
}
```

## License

GPL-3.0-only
