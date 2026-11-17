//! The audit layer: what it stores, when, and which locks it holds meanwhile.

use super::support::{Reply, ok, response, serve};
use crate::audit::Auditor;
use crate::{APIClient, APIClientError, AuditConfig};
use futures::FutureExt;
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Records the writes and can slow or block them. It overrides only
/// `write_audit_data`, so every record must arrive there.
#[derive(Default)]
struct Rec {
    writes: Mutex<Vec<(String, Vec<u8>)>>,
    /// Added to every write whose path ends with `_request.txt`.
    request_delay: Duration,
    /// Added to every write.
    delay: Duration,
    /// A response write waits for this before it is stored.
    response_gate: Option<Arc<Notify>>,
    /// Set when a response write starts.
    response_started: Arc<AtomicBool>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl Rec {
    fn stored(&self, suffix: &str) -> Vec<(String, Vec<u8>)> {
        self.writes
            .lock()
            .expect("lock")
            .iter()
            .filter(|(p, _)| p.ends_with(suffix))
            .cloned()
            .collect()
    }

    fn count(&self) -> usize {
        self.writes.lock().expect("lock").len()
    }
}

struct Handle(Arc<Rec>);

impl Auditor for Handle {
    fn write_audit_data(
        &self,
        path: &str,
        data: &[u8],
    ) -> BoxFuture<'static, Result<(), APIClientError>> {
        let rec = self.0.clone();
        let path = path.to_owned();
        let data = data.to_vec();
        async move {
            let now = rec.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            rec.max_in_flight.fetch_max(now, Ordering::SeqCst);
            if path.ends_with("_request.txt") {
                tokio::time::sleep(rec.request_delay).await;
            } else {
                rec.response_started.store(true, Ordering::SeqCst);
                if let Some(gate) = &rec.response_gate {
                    gate.notified().await;
                }
            }
            tokio::time::sleep(rec.delay).await;
            rec.writes.lock().expect("lock").push((path, data));
            rec.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        .boxed()
    }
}

fn client(url: &str, rec: &Arc<Rec>) -> crate::builder::APIClientBuilder {
    APIClient::new(url).with_auditor(Arc::new(Handle(rec.clone())))
}

#[tokio::test]
async fn a_request_without_audit_config_touches_no_auditor() {
    let server = serve(|_, _| ok("hello")).await;
    let rec = Arc::new(Rec::default());
    let client = client(&server.url, &rec).build().expect("client");

    let resp = client
        .post("/plain")
        .body("payload")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.bytes().await.expect("bytes"), "hello");
    assert_eq!(rec.count(), 0);
}

#[tokio::test]
async fn the_request_record_is_written_while_the_server_answers() {
    let server =
        serve(|_, _| Reply::After(Duration::from_millis(200), response("200 OK", &[], "ok"))).await;
    let rec = Arc::new(Rec {
        request_delay: Duration::from_millis(200),
        ..Rec::default()
    });
    let client = client(&server.url, &rec).build().expect("client");

    let started = Instant::now();
    let resp = client
        .get("/slow")
        .audit(AuditConfig::new("t"))
        .send()
        .await
        .expect("send");
    let took = started.elapsed();
    assert_eq!(resp.bytes().await.expect("bytes"), "ok");

    assert!(took < Duration::from_millis(350), "took {took:?}");
    assert_eq!(rec.stored("_request.txt").len(), 1);
    assert_eq!(rec.stored("_response.txt").len(), 1);
}

#[tokio::test]
async fn a_cancelled_call_still_gets_its_request_record() {
    let server = serve(|_, _| Reply::Hang).await;
    let rec = Arc::new(Rec {
        request_delay: Duration::from_millis(200),
        ..Rec::default()
    });
    let client = client(&server.url, &rec).build().expect("client");

    let call = client.get("/cancelled").audit(AuditConfig::new("t")).send();
    let outcome = tokio::time::timeout(Duration::from_millis(50), call).await;
    assert!(outcome.is_err(), "the call must still be waiting");

    let deadline = Instant::now() + Duration::from_secs(3);
    while rec.stored("_request.txt").is_empty() {
        assert!(Instant::now() < deadline, "the request record never came");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(rec.stored("_response.txt").len(), 0);
}

#[tokio::test]
async fn the_permit_is_free_while_the_response_record_is_written() {
    let server = serve(|_, _| ok("x")).await;
    let gate = Arc::new(Notify::new());
    let rec = Arc::new(Rec {
        response_gate: Some(gate.clone()),
        ..Rec::default()
    });
    let client = client(&server.url, &rec)
        .max_concurrent(Some(1))
        .build()
        .expect("client");

    let first = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .get("/first")
                .audit(AuditConfig::new("t"))
                .send()
                .await
                .map(|_| ())
        })
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while !rec.response_started.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the response write never began");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The first call sits in its response write, the only permit is free.
    let second = tokio::time::timeout(Duration::from_secs(3), client.get("/second").send())
        .await
        .expect("the second request waited for the permit")
        .expect("send");
    assert_eq!(second.bytes().await.expect("bytes"), "x");
    assert_eq!(server.hits(), 2);
    assert!(!first.is_finished());

    gate.notify_one();
    first.await.expect("join").expect("first");
    assert_eq!(rec.stored("_response.txt").len(), 1);
}

#[tokio::test]
async fn the_audit_write_limit_bounds_the_writes_in_flight() {
    let server = serve(|_, _| ok("x")).await;
    let rec = Arc::new(Rec {
        delay: Duration::from_millis(40),
        ..Rec::default()
    });
    let client = client(&server.url, &rec)
        .max_concurrent(Some(8))
        .audit_write_limit(Some(1))
        .build()
        .expect("client");

    let calls = (0..4).map(|i| {
        let client = client.clone();
        async move {
            client
                .get(&format!("/n{i}"))
                .audit(AuditConfig::new("t"))
                .send()
                .await
                .expect("send")
                .bytes()
                .await
                .expect("bytes")
        }
    });
    futures::future::join_all(calls).await;

    assert_eq!(rec.count(), 8);
    assert_eq!(rec.max_in_flight.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn without_a_limit_writes_overlap() {
    let server = serve(|_, _| ok("x")).await;
    let rec = Arc::new(Rec {
        delay: Duration::from_millis(100),
        ..Rec::default()
    });
    let client = client(&server.url, &rec)
        .max_concurrent(Some(8))
        .audit_write_limit(None)
        .build()
        .expect("client");

    let calls = (0..4).map(|i| {
        let client = client.clone();
        async move {
            client
                .get(&format!("/n{i}"))
                .audit(AuditConfig::new("t"))
                .send()
                .await
                .expect("send");
        }
    });
    futures::future::join_all(calls).await;
    assert!(rec.max_in_flight.load(Ordering::SeqCst) > 1);
}

#[tokio::test]
async fn a_retried_get_stores_a_record_pair_per_attempt() {
    let server = serve(|n, _| {
        if n < 2 {
            Reply::Raw(response("503 Service Unavailable", &[], ""))
        } else {
            ok("done")
        }
    })
    .await;
    let rec = Arc::new(Rec::default());
    let client = client(&server.url, &rec).build().expect("client");

    let resp = client
        .get("/flaky")
        .audit(AuditConfig::new("t"))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.bytes().await.expect("bytes"), "done");

    let requests = rec.stored("_request.txt");
    let responses = rec.stored("_response.txt");
    assert_eq!(requests.len(), 3);
    assert_eq!(responses.len(), 3);
    // Every attempt writes under the same key.
    assert!(requests.iter().all(|(p, _)| *p == requests[0].0));
    assert!(responses.iter().all(|(p, _)| *p == responses[0].0));
}

#[tokio::test]
async fn a_muted_response_stores_only_the_request() {
    let server = serve(|_, _| ok("body")).await;
    let rec = Arc::new(Rec::default());
    let client = client(&server.url, &rec).build().expect("client");

    let resp = client
        .get("/muted")
        .audit(AuditConfig::new("t").mute_response())
        .send()
        .await
        .expect("send");
    assert_eq!(resp.bytes().await.expect("bytes"), "body");
    assert_eq!(rec.stored("_request.txt").len(), 1);
    assert_eq!(rec.stored("_response.txt").len(), 0);
}

#[tokio::test]
async fn a_failed_call_still_gets_its_request_record() {
    let server = serve(|_, _| Reply::Hang).await;
    let rec = Arc::new(Rec {
        request_delay: Duration::from_millis(100),
        ..Rec::default()
    });
    let client = client(&server.url, &rec)
        .retry(crate::RetryPolicy::none())
        .build()
        .expect("client");

    let err = client
        .get("/hang")
        .timeout(Duration::from_millis(50))
        .audit(AuditConfig::new("t"))
        .send()
        .await;
    assert!(err.is_err());
    // The error is returned only after the record is written.
    assert_eq!(rec.stored("_request.txt").len(), 1);
}

#[tokio::test]
async fn a_single_piece_is_written_through_write_audit_data() {
    use bytes::Bytes;

    struct Probe {
        data: Mutex<Vec<Vec<u8>>>,
        owned: AtomicBool,
    }
    struct ProbeAuditor(Arc<Probe>);
    impl Auditor for ProbeAuditor {
        fn write_audit_data(
            &self,
            _path: &str,
            data: &[u8],
        ) -> BoxFuture<'static, Result<(), APIClientError>> {
            self.0.data.lock().expect("lock").push(data.to_vec());
            async { Ok(()) }.boxed()
        }

        fn write_audit_owned(
            &self,
            _path: &str,
            _data: Vec<u8>,
        ) -> BoxFuture<'static, Result<(), APIClientError>> {
            self.0.owned.store(true, Ordering::SeqCst);
            async { Ok(()) }.boxed()
        }
    }

    let probe = Arc::new(Probe {
        data: Mutex::new(Vec::new()),
        owned: AtomicBool::new(false),
    });
    let auditor = ProbeAuditor(probe.clone());
    auditor
        .write_audit_chunks("p", vec![Bytes::from_static(b"one piece")])
        .await
        .expect("write");
    assert_eq!(
        *probe.data.lock().expect("lock"),
        vec![b"one piece".to_vec()]
    );
    assert!(!probe.owned.load(Ordering::SeqCst));

    // Several pieces are joined and take the owned route.
    auditor
        .write_audit_chunks(
            "p",
            vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")],
        )
        .await
        .expect("write");
    assert!(probe.owned.load(Ordering::SeqCst));
}

#[test]
fn the_request_id_is_twelve_lowercase_hex_digits() {
    let meta = crate::AuditMetadata::new("n", "/u");
    assert_eq!(meta.request_id.len(), 12);
    assert!(
        meta.request_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}

#[test]
fn the_audit_path_layout_is_unchanged() {
    use crate::Method;
    let mut meta = crate::AuditMetadata::new("name", "//a//b/");
    meta.date_path = "2026/01/02".into();
    meta.timestamp = "260102_030405".into();
    meta.request_id = "0123456789ab".into();
    assert_eq!(
        meta.path(Method::Put, "request"),
        "2026/01/02/name/a/b/PUT_260102_030405_0123456789ab_request.txt"
    );
    let meta = meta.with_root_path(Some("/root/x/".into()));
    assert_eq!(
        meta.path(Method::Get, "response"),
        "root/x/2026/01/02/name/a/b/GET_260102_030405_0123456789ab_response.txt"
    );
}

/// Response bodies too large (or of unknown length) to buffer: the head-only
/// record, the tee, `flush_audit`.
mod streamed {
    use super::*;
    use bytes::Bytes;
    use futures::StreamExt;
    use std::future::Future;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Calls `handler` with each accepted connection, after its request head.
    async fn raw_server<F, Fut>(handler: F) -> String
    where
        F: Fn(TcpStream) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let handler = Arc::new(handler);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let handler = handler.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    handler(stream).await;
                });
            }
        });
        url
    }

    /// Writes `head` and `body` at once, then keeps the socket open.
    async fn reply(mut stream: TcpStream, head: &str, body: &[u8]) {
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(body).await;
        let _ = stream.flush().await;
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest).await;
    }

    async fn call(client: &APIClient, cfg: AuditConfig) -> (Vec<u8>, usize) {
        let resp = client.get("/big").audit(cfg).send().await.expect("send");
        let mut stream = resp.bytes_stream().expect("stream");
        let mut got = Vec::new();
        let mut pieces = 0;
        while let Some(piece) = stream.next().await {
            got.extend_from_slice(&piece.expect("piece"));
            pieces += 1;
        }
        (got, pieces)
    }

    fn text(rec: &Rec, suffix: &str) -> String {
        let stored = rec.stored(suffix);
        assert_eq!(stored.len(), 1, "{suffix}");
        String::from_utf8(stored[0].1.clone()).expect("utf8")
    }

    #[tokio::test]
    async fn a_body_up_to_the_limit_is_buffered_and_stored_as_received() {
        let server = serve(|_, _| ok("0123456789")).await;
        let rec = Arc::new(Rec::default());
        let client = client(&server.url, &rec)
            .audit_body_limit(10)
            .build()
            .expect("client");
        let (got, _) = call(&client, AuditConfig::new("t")).await;
        assert_eq!(got, b"0123456789");
        assert_eq!(text(&rec, "_response.txt"), "0123456789");
    }

    #[tokio::test]
    async fn a_body_over_the_limit_gets_a_head_only_record() {
        let body = "x".repeat(100);
        let server = serve(move |_, _| ok(&body)).await;
        let rec = Arc::new(Rec::default());
        let client = client(&server.url, &rec)
            .audit_body_limit(10)
            .build()
            .expect("client");
        let (got, _) = call(&client, AuditConfig::new("t")).await;
        assert_eq!(got, "x".repeat(100).into_bytes());
        let record = text(&rec, "_response.txt");
        assert!(record.starts_with("HTTP/1.1 200 OK\n"), "{record}");
        assert!(record.contains("content-length: 100\n"), "{record}");
        assert!(
            record.ends_with("\n\n[body not audited: content-length 100]\n"),
            "{record}"
        );
    }

    #[tokio::test]
    async fn a_chunked_body_has_no_length_and_gets_a_head_only_record() {
        let url = raw_server(|s| async move {
            reply(
                s,
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                b"5\r\nhello\r\n0\r\n\r\n",
            )
            .await;
        })
        .await;
        let rec = Arc::new(Rec::default());
        let client = client(&url, &rec).build().expect("client");
        let (got, _) = call(&client, AuditConfig::new("t")).await;
        assert_eq!(got, b"hello");
        let record = text(&rec, "_response.txt");
        assert!(
            record.ends_with("[body not audited: unknown length]\n"),
            "{record}"
        );
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for b in data {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A gzip stream whose deflate data is one stored block.
    fn gzip(data: &[u8]) -> Vec<u8> {
        let len = u16::try_from(data.len()).expect("small");
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255, 1];
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(data).to_le_bytes());
        out.extend_from_slice(&u32::from(len).to_le_bytes());
        out
    }

    #[cfg(feature = "gzip")]
    #[tokio::test]
    async fn a_decoded_gzip_body_has_no_length_and_gets_a_head_only_record() {
        let packed = gzip(b"small and decoded");
        let url = raw_server(move |s| {
            let packed = packed.clone();
            async move {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    packed.len()
                );
                reply(s, &head, &packed).await;
            }
        })
        .await;
        let rec = Arc::new(Rec::default());
        let client = client(&url, &rec).build().expect("client");
        let (got, _) = call(&client, AuditConfig::new("t")).await;
        assert_eq!(got, b"small and decoded");
        let record = text(&rec, "_response.txt");
        assert!(
            record.ends_with("[body not audited: unknown length]\n"),
            "{record}"
        );
    }

    #[tokio::test]
    async fn the_head_record_redacts_credentials_and_cookies() {
        let body = "x".repeat(100);
        let server = serve(move |_, _| {
            Reply::Raw(response(
                "200 OK",
                &[
                    ("Set-Cookie", "sid=cookie-secret; HttpOnly"),
                    ("Set-Cookie", "other=second-secret"),
                    ("Authorization", "Bearer token-secret"),
                    ("X-Plain", "visible"),
                ],
                &body,
            ))
        })
        .await;
        let rec = Arc::new(Rec::default());
        let client = client(&server.url, &rec)
            .audit_body_limit(10)
            .build()
            .expect("client");
        call(&client, AuditConfig::new("t")).await;
        let record = text(&rec, "_response.txt");
        assert!(!record.contains("secret"), "{record}");
        assert_eq!(
            record.matches("set-cookie: <redacted>\n").count(),
            2,
            "{record}"
        );
        assert!(record.contains("authorization: <redacted>\n"), "{record}");
        assert!(record.contains("x-plain: visible\n"), "{record}");
    }

    #[tokio::test]
    async fn a_teed_body_is_stored_whole_while_the_caller_streams_it() {
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let served = body.clone();
        let url = raw_server(move |s| {
            let served = served.clone();
            async move {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    served.len()
                );
                reply(s, &head, &served).await;
            }
        })
        .await;
        let rec = Arc::new(Rec::default());
        let client = client(&url, &rec)
            .audit_body_limit(1000)
            .build()
            .expect("client");
        let (got, _) = call(&client, AuditConfig::new("t").tee_response()).await;
        assert_eq!(got, body);
        client.flush_audit().await;
        let stored = rec.stored("_response.txt");
        assert_eq!(stored.len(), 1);
        assert!(stored[0].1 == body, "stored bytes differ");
        assert_eq!(rec.stored("_response_partial.txt").len(), 0);
    }

    #[tokio::test]
    async fn an_abandoned_tee_stores_no_response_and_a_partial_record() {
        let url = raw_server(|mut s| async move {
            let _ = s
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n",
                )
                .await;
            let _ = s.write_all(&[b'a'; 1000]).await;
            let _ = s.flush().await;
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest).await;
        })
        .await;
        let rec = Arc::new(Rec::default());
        let client = client(&url, &rec)
            .audit_body_limit(1000)
            .build()
            .expect("client");
        let resp = client
            .get("/big")
            .audit(AuditConfig::new("t").tee_response())
            .send()
            .await
            .expect("send");
        let mut stream = resp.bytes_stream().expect("stream");
        let first = stream.next().await.expect("chunk").expect("ok");
        assert!(!first.is_empty());
        drop(stream);

        tokio::time::timeout(Duration::from_secs(5), client.flush_audit())
            .await
            .expect("flush");
        assert_eq!(rec.stored("_response.txt").len(), 0);
        let partial = text(&rec, "_response_partial.txt");
        assert!(partial.starts_with("HTTP/1.1 200 OK\n"), "{partial}");
        assert!(
            partial.ends_with(&format!("[body incomplete: {} bytes seen]\n", first.len())),
            "{partial}"
        );
    }

    /// Counts what `write_audit_stream` receives; waits for `gate` first.
    #[derive(Default)]
    struct StreamRec {
        gate: Option<Arc<Notify>>,
        pieces: AtomicUsize,
        max_piece: AtomicUsize,
        total: AtomicUsize,
    }
    struct StreamHandle(Arc<StreamRec>);
    impl Auditor for StreamHandle {
        fn write_audit_data(
            &self,
            _path: &str,
            _data: &[u8],
        ) -> BoxFuture<'static, Result<(), APIClientError>> {
            async { Ok(()) }.boxed()
        }

        fn write_audit_stream<'a>(
            &'a self,
            _path: &str,
            mut body: futures::stream::BoxStream<'static, Result<Bytes, std::io::Error>>,
        ) -> BoxFuture<'a, Result<(), APIClientError>> {
            let rec = self.0.clone();
            async move {
                if let Some(gate) = &rec.gate {
                    gate.notified().await;
                }
                while let Some(piece) = body.next().await {
                    let piece = piece?;
                    rec.pieces.fetch_add(1, Ordering::SeqCst);
                    rec.max_piece.fetch_max(piece.len(), Ordering::SeqCst);
                    rec.total.fetch_add(piece.len(), Ordering::SeqCst);
                }
                Ok(())
            }
            .boxed()
        }
    }

    /// `count` chunks of `size` bytes, one HTTP chunk each, `gap` apart.
    async fn send_chunked(mut s: TcpStream, count: usize, size: usize, gap: Duration) {
        let _ = s
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await;
        let payload = vec![b'z'; size];
        for _ in 0..count {
            let _ = s.write_all(format!("{size:x}\r\n").as_bytes()).await;
            let _ = s.write_all(&payload).await;
            let _ = s.write_all(b"\r\n").await;
            let _ = s.flush().await;
            tokio::time::sleep(gap).await;
        }
        let _ = s.write_all(b"0\r\n\r\n").await;
        let _ = s.flush().await;
        let mut rest = Vec::new();
        let _ = s.read_to_end(&mut rest).await;
    }

    #[tokio::test]
    async fn a_slow_auditor_slows_the_caller_down() {
        let url = raw_server(|s| send_chunked(s, 60, 1024, Duration::from_millis(2))).await;
        let gate = Arc::new(Notify::new());
        let auditor = Arc::new(StreamRec {
            gate: Some(gate.clone()),
            ..StreamRec::default()
        });
        let client = APIClient::new(&url)
            .with_auditor(Arc::new(StreamHandle(auditor.clone())))
            .build()
            .expect("client");
        let resp = client
            .get("/slow")
            .audit(AuditConfig::new("t").tee_response())
            .send()
            .await
            .expect("send");
        let mut stream = resp.bytes_stream().expect("stream");

        // The auditor reads nothing yet: the caller stalls a few chunks in.
        let mut read = 0;
        let mut bytes = 0;
        while let Ok(piece) = tokio::time::timeout(Duration::from_millis(400), stream.next()).await
        {
            bytes += piece.expect("some").expect("ok").len();
            read += 1;
            assert!(bytes < 60 * 1024, "the caller ran to the end");
        }
        assert!((1..=10).contains(&read), "read {read} chunks ahead");

        gate.notify_one();
        while let Some(piece) = stream.next().await {
            bytes += piece.expect("piece").len();
        }
        assert_eq!(bytes, 60 * 1024);
        drop(stream);
        client.flush_audit().await;
        assert_eq!(auditor.total.load(Ordering::SeqCst), 60 * 1024);
    }

    #[tokio::test]
    async fn a_large_teed_body_is_streamed_not_held_whole() {
        const TOTAL: usize = 32 * 1024 * 1024;
        let url = raw_server(|mut s| async move {
            let head =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {TOTAL}\r\nConnection: close\r\n\r\n");
            let _ = s.write_all(head.as_bytes()).await;
            let block = vec![b'q'; 64 * 1024];
            for _ in 0..TOTAL / block.len() {
                if s.write_all(&block).await.is_err() {
                    return;
                }
            }
            let _ = s.flush().await;
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest).await;
        })
        .await;
        let auditor = Arc::new(StreamRec::default());
        let client = APIClient::new(&url)
            .with_auditor(Arc::new(StreamHandle(auditor.clone())))
            .build()
            .expect("client");
        let (got, pieces) = call(&client, AuditConfig::new("t").tee_response()).await;
        assert_eq!(got.len(), TOTAL);
        assert!(pieces > 1);
        client.flush_audit().await;
        assert_eq!(auditor.total.load(Ordering::SeqCst), TOTAL);
        assert!(auditor.pieces.load(Ordering::SeqCst) > 1);
        assert!(
            auditor.max_piece.load(Ordering::SeqCst) < 4 * 1024 * 1024,
            "a piece of {} bytes",
            auditor.max_piece.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn a_teed_body_held_unread_takes_no_audit_write_slot() {
        let server = serve(|_, req| {
            if req.contains("/held") {
                Reply::Raw(format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n{}",
                    "a".repeat(1000)
                ))
            } else {
                ok("ok")
            }
        })
        .await;
        let rec = Arc::new(Rec::default());
        let client = client(&server.url, &rec)
            .audit_body_limit(1000)
            .audit_write_limit(Some(1))
            .build()
            .expect("client");

        // The teed response stays open: its body is neither read nor dropped.
        let held = client
            .get("/held")
            .audit(AuditConfig::new("t").tee_response())
            .send()
            .await
            .expect("send");

        // With one write slot, a buffered call still stores both its records.
        tokio::time::timeout(Duration::from_secs(3), async {
            client
                .get("/two")
                .audit(AuditConfig::new("t"))
                .send()
                .await
                .expect("send")
                .bytes()
                .await
                .expect("bytes");
        })
        .await
        .expect("the second call waited for a write slot");
        assert_eq!(rec.stored("_response.txt").len(), 1);
        drop(held);
        client.flush_audit().await;
    }

    #[tokio::test]
    async fn flush_audit_returns_at_once_with_nothing_in_flight() {
        let server = serve(|_, _| ok("x")).await;
        let rec = Arc::new(Rec::default());
        let client = client(&server.url, &rec).build().expect("client");
        tokio::time::timeout(Duration::from_millis(100), client.flush_audit())
            .await
            .expect("flush waited");
    }
}
