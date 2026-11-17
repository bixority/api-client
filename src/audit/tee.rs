//! Storing a response body while the caller streams it.

use bytes::Bytes;
use futures::channel::mpsc;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use tokio::sync::Notify;

/// How many chunks the caller may be ahead of the auditor.
const CHANNEL_CAPACITY: usize = 8;

/// Counts the audit writes that outlive their call, so that they can be waited
/// for.
#[derive(Default)]
pub struct Tracker {
    running: AtomicUsize,
    idle: Notify,
}

/// Held by a running write; releasing it may wake the waiters.
pub struct TaskGuard(Arc<Tracker>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if self.0.running.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl Tracker {
    /// Register a write. Do it before the task is spawned, so that a flush
    /// that follows cannot miss it.
    pub(crate) fn start(self: &Arc<Self>) -> TaskGuard {
        self.running.fetch_add(1, Ordering::SeqCst);
        TaskGuard(self.clone())
    }

    /// Wait until no write is running.
    pub(crate) async fn wait(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            // Registered before the check, so a wake-up in between is kept.
            notified.as_mut().enable();
            if self.running.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// What the tee writer learns about the body.
pub struct TeeState {
    /// The body ended normally.
    pub(crate) done: AtomicBool,
    /// Bytes handed to the caller.
    pub(crate) seen: AtomicU64,
}

/// The body the caller reads. Each chunk is offered to the auditor's channel
/// first (waiting when it is full) and then yielded.
struct TeeBody {
    inner: BoxStream<'static, reqwest::Result<Bytes>>,
    tx: Option<mpsc::Sender<Bytes>>,
    /// A chunk read from `inner` that is not yet in the channel.
    pending: Option<Bytes>,
    state: Arc<TeeState>,
}

impl Stream for TeeBody {
    type Item = reqwest::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        loop {
            if let Some(chunk) = this.pending.take() {
                if let Some(tx) = this.tx.as_mut() {
                    match tx.poll_ready(cx) {
                        Poll::Pending => {
                            this.pending = Some(chunk);
                            return Poll::Pending;
                        }
                        Poll::Ready(Ok(())) => {
                            // Cannot fail after a successful `poll_ready`.
                            if tx.start_send(chunk.clone()).is_err() {
                                this.tx = None;
                            }
                        }
                        // The auditor stopped reading: only the caller is left.
                        Poll::Ready(Err(_)) => this.tx = None,
                    }
                }
                this.state
                    .seen
                    .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                return Poll::Ready(Some(Ok(chunk)));
            }
            match this.inner.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    if this.tx.is_some() {
                        this.pending = Some(chunk);
                    } else {
                        this.state
                            .seen
                            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    // Closed without `done`: the auditor's stream ends in an
                    // error.
                    this.tx = None;
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => {
                    this.state.done.store(true, Ordering::SeqCst);
                    this.tx = None;
                    return Poll::Ready(None);
                }
            }
        }
    }
}

/// Split a response body in two: the stream for the caller, and the stream
/// for the auditor, which ends with an `Err` unless the body ended normally.
pub fn tee(body: BoxStream<'static, reqwest::Result<Bytes>>) -> (TeeStreams, Arc<TeeState>) {
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let state = Arc::new(TeeState {
        done: AtomicBool::new(false),
        seen: AtomicU64::new(0),
    });
    let caller = TeeBody {
        inner: body,
        tx: Some(tx),
        pending: None,
        state: state.clone(),
    };
    let flag = state.clone();
    let end = futures::stream::once(std::future::ready(())).filter_map(move |()| {
        std::future::ready(
            (!flag.done.load(Ordering::SeqCst))
                .then(|| Err(std::io::Error::other("response body abandoned"))),
        )
    });
    let auditor = rx.map(Ok).chain(end).boxed();
    (
        TeeStreams {
            caller: caller.boxed(),
            auditor,
        },
        state,
    )
}

pub struct TeeStreams {
    pub(crate) caller: BoxStream<'static, reqwest::Result<Bytes>>,
    pub(crate) auditor: BoxStream<'static, Result<Bytes, std::io::Error>>,
}
