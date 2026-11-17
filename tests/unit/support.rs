//! A raw TCP HTTP/1.1 server for tests: one request per connection, replies
//! chosen by a handler.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub enum Reply {
    /// Send these bytes at once.
    Raw(String),
    /// Send these bytes after the delay.
    After(Duration, String),
    /// Never answer.
    Hang,
}

pub fn ok(body: &str) -> Reply {
    Reply::Raw(response("200 OK", &[], body))
}

pub fn response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        let _ = write!(out, "{k}: {v}\r\n");
    }
    out.push_str("\r\n");
    out.push_str(body);
    out
}

pub struct Server {
    pub url: String,
    pub hits: Arc<AtomicUsize>,
    pub requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    pub fn request(&self, i: usize) -> String {
        self.requests.lock().expect("lock")[i].clone()
    }
}

/// The request head and, by `Content-Length`, its body.
async fn read_request(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(n) = stream.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let len = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if body.len() >= len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Starts the server. `handler` gets the 0-based request number and the raw request.
pub async fn serve(handler: impl Fn(usize, &str) -> Reply + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let hits = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let (seen, log) = (hits.clone(), requests.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let (handler, hits, requests) = (handler.clone(), seen.clone(), log.clone());
            tokio::spawn(async move {
                let req = read_request(&mut stream).await;
                let n = hits.fetch_add(1, Ordering::SeqCst);
                let reply = handler(n, &req);
                requests.lock().expect("lock").push(req);
                match reply {
                    Reply::Raw(s) => {
                        let _ = stream.write_all(s.as_bytes()).await;
                        let _ = stream.flush().await;
                        // Keep the socket open until the client is done.
                        let mut rest = Vec::new();
                        let _ = stream.read_to_end(&mut rest).await;
                    }
                    Reply::After(d, s) => {
                        tokio::time::sleep(d).await;
                        let _ = stream.write_all(s.as_bytes()).await;
                        let _ = stream.flush().await;
                        let mut rest = Vec::new();
                        let _ = stream.read_to_end(&mut rest).await;
                    }
                    Reply::Hang => tokio::time::sleep(Duration::from_secs(60)).await,
                }
            });
        }
    });
    Server {
        url,
        hits,
        requests,
    }
}
