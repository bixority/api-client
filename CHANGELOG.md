# Changelog

## 0.2.0

### Breaking changes

- The audit record of a response is stored exactly as received, at every size. It was pretty-printed JSON up to 256 KiB.
- A response with no `Content-Length`, or one above `audit_body_limit` (default 4 MiB), is no longer buffered. The caller
  gets its body as a stream and the audit stores a head-only record (status line, headers without credentials and
  cookies, and a note of the length), unless the request asked for `AuditConfig::tee_response()`, which stores the whole
  body as the caller reads it.
- Response decompression (`gzip`, `brotli`, `zstd`, `deflate`) is on by default. A decoded response has no
  `Content-Length`, so its audit record is head-only. Use `default-features = false` to turn decompression off.
- The audit logs moved from INFO to DEBUG: the curl line, the response, the muted-response line and
  `Audit data written to`. Failures to write stay at WARN. A non-UTF-8 request body is logged as
  `<N bytes, not UTF-8>`.
- A request with no `AuditConfig` is not audited at all. A non-UTF-8 request body is stored lossily converted, in windows.
- `AuditLayer` is no longer public. `APIClientError` is `#[non_exhaustive]` and has new variants (such as
  `QueueTimeout`, `InvalidHeader` and `Json`).
- The tokio features are trimmed to `sync`, `time` and `rt`. A dependent that needs other tokio features declares them.
- `timeout_secs` is now the default total time of each request, from sending to the end of the body, not a client-wide
  timeout. A request can override it.
- The audit layer and its types are behind the new `audit-core` feature; `audit` adds `ObjectStorageAuditor`.
- HTTP/2 is negotiated by default with a connection window of `(max_concurrent + 2) * 2 MiB`.
- The idle connections per host default to `max_concurrent`.
- The concurrency permit is released at the response head (or, for an audited unmuted body, once it has been read)
  instead of after the audit writes. The request record is written while the request is in flight.
- `Auditor::write_audit_chunks` is the method the layer calls; override it rather than `write_audit_data` to avoid
  copies. Its default passes a single piece straight to `write_audit_data`.

### Added

- The request builder: `APIClient::get`, `post`, `put`, `patch`, `delete`, `head` and `req`, with `query`, `header`,
  `headers`, `json`, `body`, `timeout`, `no_timeout`, `audit` and `send`.
- `RetryPolicy`: attempts, delays, jitter, `Retry-After`, retried statuses and methods. The defaults match 0.1.
- `APIClientBuilder::queue_deadline`, which fails a request with `QueueTimeout` if no permit comes in time.
- `hold_permit_until_body_end`, per client and per request, to keep the permit until the body is read or dropped.
- `connect_timeout_secs`, `read_timeout`, `http1_only`, `http2_connection_window` and `pool_max_idle_per_host` defaults.
- `audit_write_limit` (default twice `max_concurrent`), `audit_body_limit`, `AuditConfig::tee_response`,
  `Auditor::write_audit_stream` and `APIClient::flush_audit`.
- `BackgroundAuditor` and `BackgroundOptions`, which write audit records in the background.
- A single `call` debug event per request with the method, status, version and the timings of the permit wait, the head,
  the body and the audit writes; it carries no URL, query or body.

### Deprecated

- `APIClient::request` and `APIClient::request_json`; use the request builder.
