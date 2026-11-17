mod background;
mod layer;
mod tee;
pub mod utils;

use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
pub use utils::{pretty_body, request_to_curl, request_to_curl_chunks, shell_quote};

pub use background::{BackgroundAuditor, BackgroundOptions};
pub(crate) use layer::AuditLayer;
pub(crate) use tee::Tracker;

use crate::types::APIClientError;
use futures::future::BoxFuture;
#[cfg(feature = "audit")]
use object_storage_client::ObjectStorageClient;
#[cfg(feature = "audit")]
use tracing::{debug, warn};

/// Where audit records go.
///
/// The audit layer calls [`Self::write_audit_chunks`] for every record, so that
/// is the method to override to store pieces without joining them. The other
/// two are what its default falls back to.
pub trait Auditor: Send + Sync {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>>;

    /// Like [`Self::write_audit_data`], but takes ownership of the data so an
    /// implementation can store it without copying. The default borrows it.
    fn write_audit_owned(
        &self,
        path: &str,
        data: Vec<u8>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        self.write_audit_data(path, &data)
    }

    /// Like [`Self::write_audit_owned`], but the data comes as consecutive
    /// pieces, so a large request body can be audited without first joining
    /// it into one buffer. The stored object is the pieces concatenated.
    ///
    /// The default passes a single piece straight to
    /// [`Self::write_audit_data`], and joins several into one `Vec` for
    /// [`Self::write_audit_owned`]; an implementation that can stream should
    /// override it.
    fn write_audit_chunks(
        &self,
        path: &str,
        chunks: Vec<Bytes>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        if let [piece] = chunks.as_slice() {
            return self.write_audit_data(path, piece);
        }
        let total = chunks.iter().map(Bytes::len).sum();
        let mut data = Vec::with_capacity(total);
        for c in &chunks {
            data.extend_from_slice(c);
        }
        drop(chunks);
        self.write_audit_owned(path, data)
    }

    /// Store a response body that arrives piece by piece, while the caller
    /// still reads it (see [`AuditConfig::tee_response`](crate::AuditConfig::tee_response)).
    ///
    /// The stream ends with an `Err` when the body did not end normally (the
    /// caller dropped it, or it failed): the implementation must then store
    /// nothing, or abort an upload in progress, and return an error. The
    /// stream is fed through a bounded channel, so a slow implementation slows
    /// the caller down.
    ///
    /// The default collects the pieces and calls [`Self::write_audit_chunks`],
    /// so it holds the whole body in memory; override it to stream.
    fn write_audit_stream<'a>(
        &'a self,
        path: &str,
        mut body: BoxStream<'static, Result<Bytes, std::io::Error>>,
    ) -> BoxFuture<'a, Result<(), APIClientError>> {
        let path = path.to_owned();
        async move {
            let mut chunks = Vec::new();
            while let Some(piece) = body.next().await {
                chunks.push(piece?);
            }
            self.write_audit_chunks(&path, chunks).await
        }
        .boxed()
    }
}

/// Auditor that writes audit data to object storage.
#[cfg(feature = "audit")]
#[allow(dead_code)]
#[derive(Clone)]
pub struct ObjectStorageAuditor {
    client: ObjectStorageClient,
}

#[cfg(feature = "audit")]
impl ObjectStorageAuditor {
    /// Create a new `ObjectStorageAuditor`.
    #[must_use]
    pub const fn new(client: ObjectStorageClient) -> Self {
        Self { client }
    }
}

#[cfg(feature = "audit")]
impl Auditor for ObjectStorageAuditor {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        self.write_audit_owned(path, data.to_vec())
    }

    fn write_audit_owned(
        &self,
        path: &str,
        data: Vec<u8>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let path = path.to_owned();
        let data_len = data.len();
        let client = self.client.clone();
        async move {
            match client.put(&path, data).await {
                Ok(()) => debug!("Audit data written to {}: {} bytes", path, data_len),
                Err(e) => warn!("Failed to write audit data to {}: {}", path, e),
            }
            // We don't want to fail the request if auditing fails
            Ok(())
        }
        .boxed()
    }

    fn write_audit_chunks(
        &self,
        path: &str,
        chunks: Vec<Bytes>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        if let [piece] = chunks.as_slice()
            && is_http(path)
        {
            return self.write_audit_data(path, piece);
        }
        let path = path.to_owned();
        let data_len: usize = chunks.iter().map(Bytes::len).sum();
        let client = self.client.clone();
        async move {
            // The http(s) backend has no multipart upload: `put_stream`
            // would buffer the object anyway and refuses one above 1 GiB,
            // which `put` stores. So it gets the joined object, as before.
            if is_http(&path) {
                let mut data = Vec::with_capacity(data_len);
                for c in chunks {
                    data.extend_from_slice(&c);
                }
                match client.put(&path, data).await {
                    Ok(()) => debug!("Audit data written to {}: {} bytes", path, data_len),
                    Err(e) => warn!("Failed to write audit data to {}: {}", path, e),
                }
                return Ok(());
            }
            let stream = futures::stream::iter(chunks.into_iter().map(Ok));
            match client.put_stream(&path, stream).await {
                Ok(()) => debug!("Audit data written to {}: {} bytes", path, data_len),
                Err(e) => warn!("Failed to write audit data to {}: {}", path, e),
            }
            // We don't want to fail the request if auditing fails
            Ok(())
        }
        .boxed()
    }

    fn write_audit_stream<'a>(
        &'a self,
        path: &str,
        body: BoxStream<'static, Result<Bytes, std::io::Error>>,
    ) -> BoxFuture<'a, Result<(), APIClientError>> {
        use futures::TryStreamExt;
        use std::sync::atomic::{AtomicBool, Ordering};

        let path = path.to_owned();
        let client = self.client.clone();
        async move {
            // The upload is committed only when the input ends cleanly: an
            // `Err` item aborts it.
            let failed = std::sync::Arc::new(AtomicBool::new(false));
            let seen = failed.clone();
            let input = body.map_err(move |e| {
                seen.store(true, Ordering::SeqCst);
                object_storage_client::Error::from(e)
            });
            let written = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let counted = written.clone();
            let input = input.inspect_ok(move |b| {
                counted.fetch_add(b.len() as u64, Ordering::Relaxed);
            });
            let result = if is_http(&path) {
                // No multipart upload there: the body is buffered anyway.
                let mut buffered = Vec::new();
                let mut input = input;
                let mut res = Ok(());
                while let Some(piece) = input.next().await {
                    match piece {
                        Ok(b) => buffered.extend_from_slice(&b),
                        Err(e) => {
                            res = Err(e);
                            break;
                        }
                    }
                }
                match res {
                    Ok(()) => client.put(&path, buffered).await,
                    Err(e) => Err(e),
                }
            } else {
                client.put_stream(&path, input).await
            };
            match result {
                Ok(()) => debug!(
                    "Audit data written to {}: {} bytes",
                    path,
                    written.load(Ordering::Relaxed)
                ),
                Err(e) => warn!("Failed to write audit data to {}: {}", path, e),
            }
            if failed.load(Ordering::SeqCst) {
                return Err(APIClientError::Io(std::io::Error::other(
                    "the response body did not end normally; nothing was stored",
                )));
            }
            // We don't want to fail the request if auditing fails
            Ok(())
        }
        .boxed()
    }
}

/// Whether `url` is an `http(s)://` address.
#[cfg(feature = "audit")]
fn is_http(url: &str) -> bool {
    url.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}
