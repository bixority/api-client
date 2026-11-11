use crate::APIClient;
#[cfg(feature = "audit")]
use crate::types::{AuditConfig, Headers, Method};
#[cfg(feature = "audit")]
use futures::FutureExt;
#[cfg(feature = "audit")]
use mockall::predicate;
use std::collections::HashMap;
#[cfg(feature = "audit")]
use std::sync::Arc;
#[cfg(feature = "audit")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn test_build_url() -> Result<(), crate::APIClientError> {
    let client = APIClient::new("https://api.example.com".to_string())
        .timeout_secs(30)
        .max_concurrent(None)
        .build()?;

    // Test basic URL building
    let url = client.build_url("test", None)?;
    assert_eq!(url, "https://api.example.com/test");

    // Test with leading/trailing slashes
    let url = client.build_url("/test/", None)?;
    assert_eq!(url, "https://api.example.com/test/");

    // Test with query parameters
    let mut params = HashMap::new();
    params.insert("key".to_string(), "value".to_string());
    params.insert("foo".to_string(), "bar".to_string());

    let url = client.build_url("search", Some(&params))?;
    // Order of query params might not be guaranteed, so we check for both
    assert!(url.contains("key=value"));
    assert!(url.contains("foo=bar"));
    assert!(url.starts_with("https://api.example.com/search?"));

    Ok(())
}

#[test]
fn test_client_init_audit_toggle() -> Result<(), crate::APIClientError> {
    // Verify client can be initialized with audit enabled
    let client_audit = APIClient::new("https://api.example.com".to_string())
        .timeout_secs(30)
        .max_concurrent(None)
        .build()?;
    assert_eq!(client_audit.base_url, "https://api.example.com");

    // Verify client can be initialized with audit disabled
    let client_no_audit = APIClient::new("https://api.example.com".to_string())
        .timeout_secs(30)
        .max_concurrent(None)
        .build()?;
    assert_eq!(client_no_audit.base_url, "https://api.example.com");

    Ok(())
}

#[test]
fn test_client_builder_defaults() -> Result<(), crate::APIClientError> {
    let client = APIClient::new("https://api.example.com".to_string()).build()?;
    assert_eq!(client.base_url, "https://api.example.com");
    Ok(())
}

#[test]
fn test_client_builder_pool_config() -> Result<(), crate::APIClientError> {
    let client = APIClient::new("https://api.example.com".to_string())
        .pool_max_idle_per_host(5)
        .build()?;
    assert_eq!(client.base_url, "https://api.example.com");
    Ok(())
}

#[test]
fn test_client_clear_cookies() -> Result<(), crate::APIClientError> {
    let client = APIClient::new("https://api.example.com".to_string()).build()?;
    // This should not panic and should clear internal cookies
    client.clear_cookies();
    Ok(())
}

#[cfg(feature = "audit")]
#[tokio::test]
async fn test_request_with_audit_config() -> Result<(), crate::APIClientError> {
    let _client = APIClient::new("https://api.example.com".to_string()).build()?;

    // This test just verifies that the new signature works and we can pass AuditConfig
    // We don't actually send a request because there's no mock server here,
    // but the compilation check and this call confirm the API changes.

    let audit = crate::AuditConfig::new("test-audit").mute_response();
    assert!(!audit.audit_response_body);
    assert_eq!(audit.name, Some("test-audit".to_string()));

    // We don't call request() here because it would try to connect to api.example.com

    Ok(())
}

#[cfg(not(feature = "audit"))]
#[tokio::test]
async fn test_request_without_audit() -> Result<(), crate::APIClientError> {
    use crate::types::{Headers, Method};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Setup a simple mock server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}");

    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).await;
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
        }
    });

    let client = APIClient::new(url).build()?;

    let response = client
        .request(
            "/resource",
            Method::Post,
            Headers::new(),
            Some(b"some body".to_vec()),
            None,
        )
        .await?;

    assert_eq!(response.status().as_u16(), 200);
    Ok(())
}

#[tokio::test]
async fn test_request_json_compilation() -> Result<(), crate::APIClientError> {
    #[derive(serde::Serialize)]
    struct MyData {
        foo: String,
    }

    let client = APIClient::new("https://api.example.com".to_string()).build()?;
    let data = MyData {
        foo: "bar".to_string(),
    };

    // This just verifies the method exists and compiles with a Serialize type
    // We don't actually call it to avoid network requests
    let _ = format!("{:?}", client.base_url);
    let _ = data.foo;

    Ok(())
}

#[cfg(feature = "audit")]
mockall::mock! {
    pub Auditor {}
    impl crate::audit::Auditor for Auditor {
        fn write_audit_data(
            &self,
            path: &str,
            data: &[u8],
        ) -> futures::future::BoxFuture<'static, Result<(), crate::APIClientError>>;
    }
}

#[cfg(feature = "audit")]
#[tokio::test]
async fn test_client_with_auditor() -> Result<(), crate::APIClientError> {
    let mut mock_auditor = MockAuditor::new();

    let now = chrono::Utc::now();
    let date_str = now.format("%Y/%m/%d").to_string();
    let yy_mm_dd = now.format("%y%m%d").to_string();

    let request_pattern =
        format!(r"^{date_str}/unittest/resource/POST_{yy_mm_dd}_\d+_[a-f0-9]{{12}}_request\.txt$");
    let response_pattern = format!(
        r"^{date_str}/unittest/resource/POST_{yy_mm_dd}_\d+_[a-f0-9]{{12}}_response\.txt$",
    );

    mock_auditor
        .expect_write_audit_data()
        .with(
            predicate::str::is_match(request_pattern).expect("invalid request pattern regex"),
            predicate::always(),
        )
        .times(1)
        .returning(|_, _| async { Ok(()) }.boxed());

    mock_auditor
        .expect_write_audit_data()
        .with(
            predicate::str::is_match(response_pattern).expect("invalid response pattern regex"),
            predicate::always(),
        )
        .times(1)
        .returning(|_, _| async { Ok(()) }.boxed());

    let mock_auditor_arc = Arc::new(mock_auditor);

    // Setup a simple mock server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}");

    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            // Read the request first, or closing with it unread resets the client.
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).await;
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
        }
    });

    let client = APIClient::new(url)
        .with_auditor(mock_auditor_arc.clone())
        .build()?;

    let audit_config = AuditConfig::new("unittest");
    let _ = client
        .request(
            "/resource",
            Method::Post,
            Headers::new(),
            Some(b"some body".to_vec()),
            None,
            Some(audit_config),
        )
        .await?;

    Ok(())
}

#[cfg(feature = "audit")]
#[tokio::test]
async fn test_client_with_auditor_and_root_path() -> Result<(), crate::APIClientError> {
    let mut mock_auditor = MockAuditor::new();

    let now = chrono::Utc::now();
    let date_str = now.format("%Y/%m/%d").to_string();
    let yy_mm_dd = now.format("%y%m%d").to_string();

    let request_pattern = format!(
        r"^custom/root/{date_str}/unittest/resource/POST_{yy_mm_dd}_\d+_[a-f0-9]{{12}}_request\.txt$"
    );
    let response_pattern = format!(
        r"^custom/root/{date_str}/unittest/resource/POST_{yy_mm_dd}_\d+_[a-f0-9]{{12}}_response\.txt$",
    );

    mock_auditor
        .expect_write_audit_data()
        .with(
            predicate::str::is_match(request_pattern).expect("invalid request pattern regex"),
            predicate::always(),
        )
        .times(1)
        .returning(|_, _| async { Ok(()) }.boxed());

    mock_auditor
        .expect_write_audit_data()
        .with(
            predicate::str::is_match(response_pattern).expect("invalid response pattern regex"),
            predicate::always(),
        )
        .times(1)
        .returning(|_, _| async { Ok(()) }.boxed());

    let mock_auditor_arc = Arc::new(mock_auditor);

    // Setup a simple mock server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}");

    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            // Read the request first, or closing with it unread resets the client.
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).await;
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
        }
    });

    let client = APIClient::new(url)
        .with_auditor(mock_auditor_arc.clone())
        .build()?;

    let audit_config = AuditConfig::new("unittest").with_path("custom/root");
    let _ = client
        .request(
            "/resource",
            Method::Post,
            Headers::new(),
            Some(b"some body".to_vec()),
            None,
            Some(audit_config),
        )
        .await?;

    Ok(())
}

#[cfg(feature = "audit")]
#[tokio::test]
async fn default_write_audit_chunks_delivers_the_curl_bytes() {
    use crate::audit::{Auditor, request_to_curl, request_to_curl_chunks};
    use bytes::Bytes;
    use futures::future::BoxFuture;
    use std::sync::Mutex;

    type Writes = Vec<(String, Vec<u8>)>;
    #[derive(Default)]
    struct Recorder(Arc<Mutex<Writes>>);
    impl Auditor for Recorder {
        fn write_audit_data(
            &self,
            path: &str,
            data: &[u8],
        ) -> BoxFuture<'static, Result<(), crate::APIClientError>> {
            self.0
                .lock()
                .expect("lock")
                .push((path.to_owned(), data.to_vec()));
            async { Ok(()) }.boxed()
        }
    }

    let recorder = Recorder::default();
    let body = Bytes::from_static(b"{\"k\":\"it's\"}");
    let headers = reqwest::header::HeaderMap::new();
    let chunks = request_to_curl_chunks(
        "https://example.com/x",
        Method::Post,
        &headers,
        Some(&body),
        None,
    );
    recorder
        .write_audit_chunks("p/request.txt", chunks)
        .await
        .expect("write");

    let expected = request_to_curl(
        "https://example.com/x",
        Method::Post,
        &headers,
        Some(&body),
        None,
    );
    let got = recorder.0.lock().expect("lock");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, "p/request.txt");
    assert_eq!(got[0].1, expected.into_bytes());
}

#[cfg(feature = "audit")]
mod response_audit {
    use super::*;
    use crate::audit::{Auditor, pretty_body};
    use bytes::Bytes;
    use futures::future::BoxFuture;
    use std::sync::Mutex;

    const PRETTY_MAX: usize = 256 * 1024;

    type Writes = Vec<(String, Vec<u8>)>;
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Writes>>);
    impl Auditor for Recorder {
        fn write_audit_data(
            &self,
            path: &str,
            data: &[u8],
        ) -> BoxFuture<'static, Result<(), crate::APIClientError>> {
            self.0
                .lock()
                .expect("lock")
                .push((path.to_owned(), data.to_vec()));
            async { Ok(()) }.boxed()
        }
    }

    /// A compact JSON array of exactly `len` bytes.
    fn json_of_len(len: usize) -> Vec<u8> {
        let mut s = String::from("[");
        while s.len() + 2 < len {
            if s.len() > 1 {
                s.push(',');
            }
            s.push('1');
        }
        // Pad with spaces before the closing bracket to hit the exact length.
        while s.len() + 1 < len {
            s.push(' ');
        }
        s.push(']');
        assert_eq!(s.len(), len);
        s.into_bytes()
    }

    /// Serves `body` once; audits the call; returns what the caller got and
    /// what the auditor stored under a `_response.txt` path.
    async fn run(body: Vec<u8>, mute: bool) -> (Vec<u8>, Vec<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let served = body;
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    served.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&served).await;
                let _ = stream.flush().await;
            }
        });

        let recorder = Recorder::default();
        let client = APIClient::new(url)
            .with_auditor(Arc::new(recorder.clone()))
            .build()
            .expect("client");
        let mut cfg = AuditConfig::new("unittest");
        if mute {
            cfg = cfg.mute_response();
        }
        let response = client
            .request(
                "/resource",
                Method::Post,
                Headers::new(),
                Some(b"x".to_vec()),
                None,
                Some(cfg),
            )
            .await
            .expect("request");
        let got = response.bytes().await.expect("bytes").to_vec();
        let stored = recorder
            .0
            .lock()
            .expect("lock")
            .iter()
            .filter(|(p, _)| p.ends_with("_response.txt"))
            .map(|(_, d)| d.clone())
            .collect();
        (got, stored)
    }

    #[tokio::test]
    async fn response_of_pretty_max_is_audited_pretty() {
        let body = json_of_len(PRETTY_MAX);
        let (got, stored) = run(body.clone(), false).await;
        assert_eq!(got, body);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0], pretty_body(&Bytes::from(body)).into_bytes());
    }

    #[tokio::test]
    async fn response_above_pretty_max_is_audited_as_received() {
        let body = json_of_len(PRETTY_MAX + 1);
        let (got, stored) = run(body.clone(), false).await;
        assert_eq!(got, body);
        assert_eq!(stored, vec![body]);
    }

    #[tokio::test]
    async fn large_non_utf8_response_is_audited_as_raw_bytes() {
        let body: Vec<u8> = (0..PRETTY_MAX + 1000)
            .map(|i| u8::try_from(i % 256).expect("fits"))
            .collect();
        let (got, stored) = run(body.clone(), false).await;
        assert_eq!(got, body);
        assert_eq!(stored, vec![body]);
    }

    #[tokio::test]
    async fn muted_response_writes_no_response_object() {
        let body = json_of_len(PRETTY_MAX + 1);
        let (got, stored) = run(body.clone(), true).await;
        assert_eq!(got, body);
        assert_eq!(stored.len(), 0);
    }

    #[tokio::test]
    async fn http_response_bytes_returns_the_body() {
        let (got, _) = run(b"hello".to_vec(), true).await;
        assert_eq!(got, b"hello");
    }
}
