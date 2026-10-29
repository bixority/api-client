//! Container start/wait, hermetic client construction and a mock HTTP server.

use api_client::{APIClient, ObjectStorageAuditor};
use object_storage_client::ObjectStorageClient;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GARAGE_IMAGE: &str = "docker.io/dxflrs/garage";
const GARAGE_TAG: &str = "v2.5.0";
const GARAGE_S3_PORT: u16 = 3900;
const GARAGE_BUCKET: &str = "audit";
const S3_ACCESS_KEY: &str = "rootroot";
const S3_SECRET_KEY: &str = "rootrootrootroot";

const AZURITE_IMAGE: &str = "mcr.microsoft.com/azure-storage/azurite";
const AZURITE_TAG: &str = "latest";
const AZURITE_BLOB_PORT: u16 = 10000;
/// Azurite's well-known development account and key (public, not a secret).
const AZURITE_ACCOUNT: &str = "devstoreaccount1";
const AZURITE_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

const LABEL: &str = "org.bixority.api-client";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

/// A name unique per call, process and run (container names, prefixes).
pub fn unique_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{nanos:x}-{:x}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// An object store the audit can write to. A container, when present, is
/// stopped and removed when the store is dropped.
pub struct Store {
    vars: HashMap<String, String>,
    root: String,
    _container: Option<ContainerAsync<GenericImage>>,
}

impl Store {
    /// Audit root, without a trailing slash, unique to this store.
    pub fn root(&self) -> String {
        self.root.clone()
    }

    /// Settings for `ObjectStorageClient::with_vars`; no process environment.
    pub fn vars(&self) -> HashMap<String, String> {
        self.vars.clone()
    }

    /// Starts Garage with a default bucket and waits until S3 answers.
    pub async fn garage() -> Self {
        let name = format!("api-client-it-garage-{}", unique_name());
        let container = GenericImage::new(GARAGE_IMAGE, GARAGE_TAG)
            .with_exposed_port(GARAGE_S3_PORT.tcp())
            .with_wait_for(WaitFor::message_on_either_std("Creating default bucket"))
            .with_copy_to("/etc/garage.toml", include_bytes!("garage.toml").to_vec())
            .with_cmd(["/garage", "server", "--single-node", "--default-bucket"])
            .with_env_var("GARAGE_DEFAULT_ACCESS_KEY", S3_ACCESS_KEY)
            .with_env_var("GARAGE_DEFAULT_SECRET_KEY", S3_SECRET_KEY)
            .with_env_var("GARAGE_DEFAULT_BUCKET", GARAGE_BUCKET)
            .with_label(LABEL, "it-garage")
            .with_container_name(name)
            .start()
            .await
            .expect("start Garage (is a container runtime available?)");
        let port = container
            .get_host_port_ipv4(GARAGE_S3_PORT.tcp())
            .await
            .expect("Garage S3 host port");
        let vars = s3_vars(port);
        let root = format!("s3://{GARAGE_BUCKET}/{}", unique_name());
        // The log line precedes API readiness: wait for a real S3 answer.
        wait_until(|| async {
            ObjectStorageClient::with_vars(vars.clone())
                .bucket_exists(&format!("s3://{GARAGE_BUCKET}/"))
                .await
                .unwrap_or(false)
        })
        .await;
        Self {
            vars,
            root,
            _container: Some(container),
        }
    }

    /// Starts Azurite, waits for the blob service and creates a container.
    pub async fn azurite() -> Self {
        let name = format!("api-client-it-azurite-{}", unique_name());
        let container = GenericImage::new(AZURITE_IMAGE, AZURITE_TAG)
            .with_exposed_port(AZURITE_BLOB_PORT.tcp())
            .with_wait_for(WaitFor::message_on_stdout(
                "Azurite Blob service successfully listens",
            ))
            .with_cmd([
                "azurite-blob",
                "--blobHost",
                "0.0.0.0",
                "--skipApiVersionCheck",
            ])
            .with_label(LABEL, "it-azurite")
            .with_container_name(name)
            .start()
            .await
            .expect("start Azurite (is a container runtime available?)");
        let port = container
            .get_host_port_ipv4(AZURITE_BLOB_PORT.tcp())
            .await
            .expect("Azurite blob host port");
        let vars = HashMap::from([
            ("AZURE_STORAGE_ACCOUNT_NAME".into(), AZURITE_ACCOUNT.into()),
            ("AZURE_STORAGE_ACCOUNT_KEY".into(), AZURITE_KEY.into()),
            (
                "AZURE_STORAGE_ENDPOINT".into(),
                format!("http://127.0.0.1:{port}/{AZURITE_ACCOUNT}"),
            ),
            ("AZURE_ALLOW_HTTP".into(), "true".into()),
        ]);
        let azure_container = format!("it-{}", unique_name());
        let url = format!("az://{azure_container}/");
        let admin = ObjectStorageClient::with_vars(vars.clone());
        wait_until(|| async { admin.create_bucket(&url).await.is_ok() }).await;
        Self {
            vars,
            root: format!("az://{azure_container}/{}", unique_name()),
            _container: Some(container),
        }
    }

    /// An S3 endpoint nothing listens on: every audit write fails.
    pub fn unreachable_s3() -> Self {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind free port");
            listener.local_addr().expect("local addr").port()
        };
        Self {
            vars: s3_vars(port),
            root: format!("s3://{GARAGE_BUCKET}/{}", unique_name()),
            _container: None,
        }
    }
}

fn s3_vars(port: u16) -> HashMap<String, String> {
    HashMap::from([
        ("S3_ENDPOINT".into(), format!("http://127.0.0.1:{port}")),
        ("S3_REGION".into(), "eu-north-1".into()),
        ("S3_ACCESS_KEY_ID".into(), S3_ACCESS_KEY.into()),
        ("S3_SECRET_ACCESS_KEY".into(), S3_SECRET_KEY.into()),
        ("S3_ALLOW_HTTP".into(), "true".into()),
    ])
}

async fn wait_until<F, Fut>(probe: F)
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = bool> + Send,
{
    let wait = async {
        while !probe().await {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    tokio::time::timeout(STARTUP_TIMEOUT, wait)
        .await
        .expect("store not ready in time");
}

/// An `APIClient` that audits into the store described by `vars`.
pub fn audited_client(base_url: &str, vars: HashMap<String, String>) -> APIClient {
    let auditor = ObjectStorageAuditor::new(ObjectStorageClient::with_vars(vars));
    APIClient::new(base_url)
        .with_auditor(Arc::new(auditor))
        .timeout_secs(30)
        .build()
        .expect("build API client")
}

/// A local HTTP server answering every request with `200` and a fixed body.
pub struct MockServer {
    pub base_url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn mock_server(body: &'static str) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let addr = listener.local_addr().expect("mock addr");
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                // The whole request fits one read for these tiny payloads.
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    MockServer {
        base_url: format!("http://{addr}"),
        task,
    }
}
