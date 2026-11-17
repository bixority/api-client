use crate::{APIClient, APIClientBuilder, APIClientError, Headers, Method};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const MIB: u32 = 1024 * 1024;

#[test]
fn idle_cap_defaults_to_max_concurrent() {
    let b = APIClientBuilder::new("https://example.com");
    assert_eq!(b.effective_pool_max_idle_per_host(), Some(10));

    let b = APIClientBuilder::new("https://example.com").max_concurrent(Some(3));
    assert_eq!(b.effective_pool_max_idle_per_host(), Some(3));

    let b = APIClientBuilder::new("https://example.com").max_concurrent(None);
    assert_eq!(b.effective_pool_max_idle_per_host(), None);

    let b = APIClientBuilder::new("https://example.com").pool_max_idle_per_host(5);
    assert_eq!(b.effective_pool_max_idle_per_host(), Some(5));
}

#[test]
fn http2_window_defaults_from_max_concurrent() {
    let b = APIClientBuilder::new("https://example.com");
    assert_eq!(b.effective_http2_connection_window(), Some(12 * 2 * MIB));

    let b = APIClientBuilder::new("https://example.com").max_concurrent(Some(0));
    assert_eq!(b.effective_http2_connection_window(), Some(2 * 2 * MIB));
}

#[test]
fn http2_window_is_clamped() {
    let b = APIClientBuilder::new("https://example.com").max_concurrent(Some(100_000));
    assert_eq!(b.effective_http2_connection_window(), Some(i32::MAX as u32));

    let b = APIClientBuilder::new("https://example.com").max_concurrent(Some(usize::MAX));
    assert_eq!(b.effective_http2_connection_window(), Some(i32::MAX as u32));
}

#[test]
fn http2_window_none_without_limit_and_overridable() {
    let b = APIClientBuilder::new("https://example.com").max_concurrent(None);
    assert_eq!(b.effective_http2_connection_window(), None);

    let b = APIClientBuilder::new("https://example.com")
        .max_concurrent(None)
        .http2_connection_window(MIB);
    assert_eq!(b.effective_http2_connection_window(), Some(MIB));

    let b = APIClientBuilder::new("https://example.com").http2_connection_window(MIB);
    assert_eq!(b.effective_http2_connection_window(), Some(MIB));
}

#[test]
fn http2_window_is_not_applied_with_http1_only() {
    let b = APIClientBuilder::new("https://example.com")
        .http2_connection_window(MIB)
        .http1_only();
    assert_eq!(b.effective_http2_connection_window(), None);
}

#[test]
fn connect_and_read_timeout_defaults() {
    let b = APIClientBuilder::new("https://example.com");
    assert_eq!(b.connect_timeout_secs, 10);
    assert_eq!(b.read_timeout, None);

    let b = b
        .connect_timeout_secs(3)
        .read_timeout(Duration::from_secs(7));
    assert_eq!(b.connect_timeout_secs, 3);
    assert_eq!(b.read_timeout, Some(Duration::from_secs(7)));
}

/// Starts a TLS server offering ALPN `h2` and `http/1.1`, makes one request
/// through a client from `configure`, and returns the protocol the server
/// negotiated.
async fn negotiated_alpn(
    configure: impl FnOnce(APIClientBuilder) -> APIClientBuilder,
) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])?;
    let cert_der = certified.cert.der().clone();
    let key_der =
        rustls::pki_types::PrivateKeyDer::try_from(certified.signing_key.serialize_der())?;

    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert_der.clone()], key_der)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await?;
        let tls = acceptor.accept(tcp).await?;
        let alpn = tls.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
        Ok::<_, std::io::Error>(alpn)
    });

    let client = configure(
        APIClientBuilder::new(format!("https://127.0.0.1:{port}"))
            .add_root_certificate(reqwest::Certificate::from_der(&cert_der)?),
    )
    .timeout_secs(5)
    .build()?;

    // The server ends the connection after the handshake, so the request
    // itself is expected to fail; only the negotiated protocol matters.
    let _ = request(&client).await;
    Ok(server.await??)
}

#[cfg(feature = "audit-core")]
async fn request(client: &APIClient) -> Result<crate::HttpResponse, APIClientError> {
    client
        .request("/", Method::Post, Headers::new(), None, None, None)
        .await
}

#[cfg(not(feature = "audit-core"))]
async fn request(client: &APIClient) -> Result<crate::HttpResponse, APIClientError> {
    client
        .request("/", Method::Post, Headers::new(), None, None)
        .await
}

#[tokio::test]
async fn client_negotiates_http2_by_default() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(negotiated_alpn(|b| b).await?, Some(b"h2".to_vec()));
    Ok(())
}

#[tokio::test]
async fn client_negotiates_http1_with_http1_only() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        negotiated_alpn(APIClientBuilder::http1_only).await?,
        Some(b"http/1.1".to_vec())
    );
    Ok(())
}
