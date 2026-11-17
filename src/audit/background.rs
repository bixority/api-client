//! An auditor that writes in the background.

use super::Auditor;
use crate::types::APIClientError;
use bytes::Bytes;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, warn};

/// Limits of a [`BackgroundAuditor`].
#[derive(Clone, Copy, Debug)]
pub struct BackgroundOptions {
    /// The most bytes of records held at once, queued or being written. A
    /// record that would go over it is dropped. Default 64 MiB.
    pub queue_bytes: usize,
    /// How many records are written at once. Default 16.
    pub writers: usize,
    /// How long one write may take before it is given up. Default 30 s.
    pub write_timeout: Duration,
}

impl Default for BackgroundOptions {
    fn default() -> Self {
        Self {
            queue_bytes: 64 * 1024 * 1024,
            writers: 16,
            write_timeout: Duration::from_secs(30),
        }
    }
}

struct Record {
    path: String,
    chunks: Vec<Bytes>,
    size: usize,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Record>,
    /// Bytes of the queued records and of those being written.
    held_bytes: usize,
    in_flight: usize,
    closed: bool,
}

struct Shared {
    inner: Arc<dyn Auditor>,
    options: BackgroundOptions,
    state: Mutex<State>,
    /// A record was queued, or the auditor was closed.
    work: Notify,
    /// The queue ran empty with nothing in flight.
    idle: Notify,
    started: AtomicBool,
    dropped: AtomicU64,
    timed_out: AtomicU64,
}

/// Closes the auditor when the last handle goes: the writers finish the queue
/// and end.
struct CloseOnDrop(Arc<Shared>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.closed = true;
        }
        for _ in 0..self.0.options.writers.max(1) {
            self.0.work.notify_one();
        }
    }
}

/// An [`Auditor`] that hands every record to a pool of background writers and
/// returns at once, so a slow or failing store never delays a request.
///
/// The records wait in a queue bounded by bytes
/// ([`BackgroundOptions::queue_bytes`]); one that does not fit is dropped and
/// counted ([`Self::dropped`]), with a warning for the first drop and every
/// 1000th after it. Each write is given [`BackgroundOptions::write_timeout`]
/// ([`Self::timed_out`]). The writers start on the first record, so a Tokio
/// runtime is needed from then on. A clone is another handle to the same
/// queue.
///
/// What it costs, against writing in the call:
/// - records still queued are lost when the process is killed or crashes; call
///   [`Self::flush`] before a clean shutdown;
/// - the request record is no longer stored before the request is sent;
/// - the records of retried attempts are written concurrently and may land
///   in any order (their keys are the same, so the last writer wins).
///
/// [`Auditor::write_audit_stream`] (a body stored with
/// [`AuditConfig::tee_response`](crate::AuditConfig::tee_response)) goes to the
/// inner auditor directly: it is already detached from the call by the layer,
/// and the channel bounds its memory.
///
/// ```no_run
/// # use api_client::{APIClient, BackgroundAuditor, BackgroundOptions, Auditor};
/// # use std::sync::Arc;
/// # async fn demo(store: Arc<dyn Auditor>) -> Result<(), api_client::APIClientError> {
/// let auditor = BackgroundAuditor::new(store, BackgroundOptions::default());
/// let client = APIClient::new("https://api.example.com")
///     .with_auditor(Arc::new(auditor.clone()))
///     .build()?;
/// // ... requests ...
/// auditor.flush().await; // before shutdown
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct BackgroundAuditor {
    shared: Arc<Shared>,
    _close: Arc<CloseOnDrop>,
}

impl BackgroundAuditor {
    #[must_use]
    pub fn new(inner: Arc<dyn Auditor>, options: BackgroundOptions) -> Self {
        let shared = Arc::new(Shared {
            inner,
            options,
            state: Mutex::new(State::default()),
            work: Notify::new(),
            idle: Notify::new(),
            started: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            timed_out: AtomicU64::new(0),
        });
        Self {
            _close: Arc::new(CloseOnDrop(shared.clone())),
            shared,
        }
    }

    /// How many records were dropped because the queue was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// How many writes were given up after the write timeout.
    #[must_use]
    pub fn timed_out(&self) -> u64 {
        self.shared.timed_out.load(Ordering::Relaxed)
    }

    /// Wait until the queue is empty and no write is in flight.
    pub async fn flush(&self) {
        loop {
            let notified = self.shared.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let busy = self
                .shared
                .state
                .lock()
                .is_ok_and(|s| !s.queue.is_empty() || s.in_flight > 0);
            if !busy {
                return;
            }
            notified.await;
        }
    }

    fn enqueue(&self, path: &str, chunks: Vec<Bytes>) {
        let size: usize = chunks.iter().map(Bytes::len).sum();
        let accepted = self.shared.state.lock().is_ok_and(|mut state| {
            let fits = size <= self.shared.options.queue_bytes
                && state.held_bytes <= self.shared.options.queue_bytes - size;
            if fits {
                state.held_bytes += size;
                state.queue.push_back(Record {
                    path: path.to_owned(),
                    chunks,
                    size,
                });
            }
            fits
        });
        if !accepted {
            let n = self.shared.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 1000 == 1 {
                warn!(
                    "audit queue full: {n} record(s) dropped so far (latest {path}, {size} bytes)"
                );
            }
            return;
        }
        self.start_writers();
        self.shared.work.notify_one();
    }

    fn start_writers(&self) {
        if self
            .shared
            .started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        for _ in 0..self.shared.options.writers.max(1) {
            tokio::spawn(writer(self.shared.clone()));
        }
    }
}

async fn writer(shared: Arc<Shared>) {
    loop {
        let next = {
            let Ok(mut state) = shared.state.lock() else {
                return;
            };
            let next = state.queue.pop_front();
            if next.is_some() {
                state.in_flight += 1;
            } else if state.closed {
                return;
            }
            next
        };
        let Some(record) = next else {
            shared.work.notified().await;
            continue;
        };

        let write = shared.inner.write_audit_chunks(&record.path, record.chunks);
        match tokio::time::timeout(shared.options.write_timeout, write).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => debug!("audit write to {} failed: {e}", record.path),
            Err(_) => {
                shared.timed_out.fetch_add(1, Ordering::Relaxed);
                warn!(
                    "audit write to {} timed out after {:?}",
                    record.path, shared.options.write_timeout
                );
            }
        }

        if let Ok(mut state) = shared.state.lock() {
            state.held_bytes = state.held_bytes.saturating_sub(record.size);
            state.in_flight -= 1;
            if state.queue.is_empty() && state.in_flight == 0 {
                shared.idle.notify_waiters();
            }
        }
    }
}

impl Auditor for BackgroundAuditor {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        self.enqueue(path, vec![Bytes::copy_from_slice(data)]);
        async { Ok(()) }.boxed()
    }

    fn write_audit_owned(
        &self,
        path: &str,
        data: Vec<u8>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        self.enqueue(path, vec![Bytes::from(data)]);
        async { Ok(()) }.boxed()
    }

    fn write_audit_chunks(
        &self,
        path: &str,
        chunks: Vec<Bytes>,
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        self.enqueue(path, chunks);
        async { Ok(()) }.boxed()
    }

    fn write_audit_stream<'a>(
        &'a self,
        path: &str,
        body: BoxStream<'static, Result<Bytes, std::io::Error>>,
    ) -> BoxFuture<'a, Result<(), APIClientError>> {
        self.shared.inner.write_audit_stream(path, body)
    }
}
