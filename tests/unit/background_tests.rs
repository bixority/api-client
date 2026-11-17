use crate::APIClientError;
use crate::audit::{Auditor, BackgroundAuditor, BackgroundOptions};
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// The store behind the background auditor.
#[derive(Default)]
struct Inner {
    /// Every write takes a permit from this when set.
    gate: Option<Arc<Semaphore>>,
    delay: Duration,
    written: Mutex<Vec<String>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    streamed: AtomicBool,
}

struct InnerHandle(Arc<Inner>);

impl Auditor for InnerHandle {
    fn write_audit_data(
        &self,
        path: &str,
        _data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let inner = self.0.clone();
        let path = path.to_owned();
        async move {
            let now = inner.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            inner.max_in_flight.fetch_max(now, Ordering::SeqCst);
            if let Some(gate) = &inner.gate {
                gate.acquire().await.expect("open").forget();
            }
            tokio::time::sleep(inner.delay).await;
            inner.written.lock().expect("lock").push(path);
            inner.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        .boxed()
    }

    fn write_audit_stream<'a>(
        &'a self,
        _path: &str,
        mut body: futures::stream::BoxStream<'static, Result<Bytes, std::io::Error>>,
    ) -> BoxFuture<'a, Result<(), APIClientError>> {
        async move {
            while let Some(piece) = body.next().await {
                piece?;
            }
            self.0.streamed.store(true, Ordering::SeqCst);
            Ok(())
        }
        .boxed()
    }
}

fn background(inner: &Arc<Inner>, options: BackgroundOptions) -> BackgroundAuditor {
    BackgroundAuditor::new(Arc::new(InnerHandle(inner.clone())), options)
}

#[tokio::test]
async fn enqueue_returns_at_once_while_the_store_is_stalled() {
    let gate = Arc::new(Semaphore::new(0));
    let inner = Arc::new(Inner {
        gate: Some(gate.clone()),
        ..Inner::default()
    });
    let auditor = background(&inner, BackgroundOptions::default());

    let started = Instant::now();
    for i in 0..5 {
        auditor
            .write_audit_chunks(&format!("p{i}"), vec![Bytes::from_static(b"data")])
            .await
            .expect("write");
    }
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(inner.written.lock().expect("lock").len(), 0);

    gate.add_permits(5);
    auditor.flush().await;
    assert_eq!(inner.written.lock().expect("lock").len(), 5);
}

#[tokio::test]
async fn the_byte_budget_drops_and_counts() {
    let gate = Arc::new(Semaphore::new(0));
    let inner = Arc::new(Inner {
        gate: Some(gate.clone()),
        ..Inner::default()
    });
    let auditor = background(
        &inner,
        BackgroundOptions {
            queue_bytes: 100,
            writers: 1,
            ..BackgroundOptions::default()
        },
    );

    // Two of 40 bytes fit; the third (120 in all) does not; neither does one
    // bigger than the whole budget.
    for i in 0..3 {
        auditor
            .write_audit_owned(&format!("p{i}"), vec![0u8; 40])
            .await
            .expect("write");
    }
    auditor
        .write_audit_owned("huge", vec![0u8; 101])
        .await
        .expect("write");
    assert_eq!(auditor.dropped(), 2);

    gate.add_permits(2);
    auditor.flush().await;
    assert_eq!(
        *inner.written.lock().expect("lock"),
        vec!["p0".to_owned(), "p1".to_owned()]
    );
    // The budget is free again once the writes are done.
    auditor
        .write_audit_owned("again", vec![0u8; 100])
        .await
        .expect("write");
    assert_eq!(auditor.dropped(), 2);
    gate.add_permits(1);
    auditor.flush().await;
}

#[tokio::test]
async fn a_write_past_the_timeout_is_counted() {
    let inner = Arc::new(Inner {
        gate: Some(Arc::new(Semaphore::new(0))),
        ..Inner::default()
    });
    let auditor = background(
        &inner,
        BackgroundOptions {
            write_timeout: Duration::from_millis(50),
            ..BackgroundOptions::default()
        },
    );
    auditor.write_audit_data("slow", b"x").await.expect("write");
    tokio::time::timeout(Duration::from_secs(3), auditor.flush())
        .await
        .expect("flush");
    assert_eq!(auditor.timed_out(), 1);
    assert_eq!(inner.written.lock().expect("lock").len(), 0);
}

#[tokio::test]
async fn flush_waits_for_the_queued_writes() {
    let inner = Arc::new(Inner {
        delay: Duration::from_millis(100),
        ..Inner::default()
    });
    let auditor = background(&inner, BackgroundOptions::default());
    for i in 0..3 {
        auditor
            .write_audit_data(&format!("p{i}"), b"x")
            .await
            .expect("write");
    }
    assert_eq!(inner.written.lock().expect("lock").len(), 0);
    auditor.flush().await;
    assert_eq!(inner.written.lock().expect("lock").len(), 3);
}

#[tokio::test]
async fn flush_returns_at_once_when_idle() {
    let inner = Arc::new(Inner::default());
    let auditor = background(&inner, BackgroundOptions::default());
    tokio::time::timeout(Duration::from_millis(100), auditor.flush())
        .await
        .expect("flush");
}

#[tokio::test]
async fn at_most_writers_writes_run_at_once() {
    let inner = Arc::new(Inner {
        delay: Duration::from_millis(30),
        ..Inner::default()
    });
    let auditor = background(
        &inner,
        BackgroundOptions {
            writers: 2,
            ..BackgroundOptions::default()
        },
    );
    for i in 0..8 {
        auditor
            .write_audit_data(&format!("p{i}"), b"x")
            .await
            .expect("write");
    }
    auditor.flush().await;
    assert_eq!(inner.written.lock().expect("lock").len(), 8);
    assert_eq!(inner.max_in_flight.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_stream_method_goes_to_the_inner_auditor() {
    let inner = Arc::new(Inner::default());
    let auditor = background(&inner, BackgroundOptions::default());
    let body = futures::stream::iter(vec![
        Ok(Bytes::from_static(b"a")),
        Ok(Bytes::from_static(b"b")),
    ])
    .boxed();
    auditor.write_audit_stream("p", body).await.expect("stream");
    assert!(inner.streamed.load(Ordering::SeqCst));
    assert_eq!(inner.written.lock().expect("lock").len(), 0);
}
