use super::Auditor;
use super::tee::{Tracker, tee};
use super::utils::{
    build_curl, cookie_header_for, head_record, log_body, not_audited_note, request_to_curl_chunks,
};
use crate::cookies::CookieJar;
use crate::types::{
    APIClientError, AuditMetadata, AuditTimings, BodyAudit, Call, Exchange, HttpRequest, Method,
};
use bytes::Bytes;
use futures::StreamExt;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tower::{Layer, Service};
use tracing::{debug, error, warn};

/// Audit layer (middleware factory).
#[derive(Clone)]
pub struct AuditLayer {
    cookies: Arc<CookieJar>,
    auditor: Option<Arc<dyn Auditor>>,
    /// Bounds the audit writes in flight; `None` is unbounded.
    writes: Option<Arc<Semaphore>>,
    /// The largest response body read whole to be audited.
    body_limit: u64,
    /// The tee writes in flight.
    tracker: Arc<Tracker>,
}

impl AuditLayer {
    pub const fn new(
        cookies: Arc<CookieJar>,
        auditor: Option<Arc<dyn Auditor>>,
        writes: Option<Arc<Semaphore>>,
        body_limit: u64,
        tracker: Arc<Tracker>,
    ) -> Self {
        Self {
            cookies,
            auditor,
            writes,
            body_limit,
            tracker,
        }
    }
}

/// Middleware service wrapper.
#[derive(Clone)]
pub struct Audit<S> {
    inner: S,
    layer: AuditLayer,
}

impl<S> Layer<S> for AuditLayer {
    type Service = Audit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Audit {
            inner,
            layer: self.clone(),
        }
    }
}

/// What a call that is stored needs.
struct Store {
    auditor: Arc<dyn Auditor>,
    meta: AuditMetadata,
    writes: Option<Arc<Semaphore>>,
}

/// Whole milliseconds, saturating.
fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// A slot for one audit write. `None` when there is no limit (or the limiter
/// was closed, which nothing does).
async fn write_slot(writes: Option<&Arc<Semaphore>>) -> Option<OwnedSemaphorePermit> {
    writes?.clone().acquire_owned().await.ok()
}

impl Store {
    /// Write one record under `path`, holding a write slot while it is
    /// written; returns how long the write itself took.
    async fn write(&self, path: &str, chunks: Vec<Bytes>) -> Duration {
        let _slot = write_slot(self.writes.as_ref()).await;
        let started = Instant::now();
        let _ = self.auditor.write_audit_chunks(path, chunks).await;
        started.elapsed()
    }
}

impl<S> Service<Call> for Audit<S>
where
    S: Service<Call, Response = Exchange, Error = APIClientError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    type Response = Exchange;
    type Error = APIClientError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut call: Call) -> Self::Future {
        let to_store = self.layer.auditor.is_some() && call.req.audit_meta.is_some();
        let to_log =
            !to_store && call.req.audit.is_some() && tracing::event_enabled!(tracing::Level::DEBUG);
        if !to_store && !to_log {
            return Box::pin(self.inner.call(call));
        }

        let store = if to_store {
            self.layer
                .auditor
                .clone()
                .zip(call.req.audit_meta.take())
                .map(|(auditor, meta)| Store {
                    auditor,
                    meta,
                    writes: self.layer.writes.clone(),
                })
        } else {
            None
        };
        let clone = self.inner.clone();
        let inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(audited(inner, call, store, self.layer.clone()))
    }
}

/// The request record being written by a task of its own, so that it ends
/// even when the caller's future is dropped, and overlaps the exchange.
type RequestWrite = JoinHandle<Duration>;

/// What the response handling of one call works with.
struct Ctx {
    method: Method,
    log_prefix: String,
    store: Option<Arc<Store>>,
    request_write: Option<RequestWrite>,
    timings: AuditTimings,
    hold: bool,
    tee: bool,
    body_limit: u64,
    tracker: Arc<Tracker>,
}

async fn audited<S>(
    mut inner: S,
    call: Call,
    store: Option<Store>,
    layer: AuditLayer,
) -> Result<Exchange, APIClientError>
where
    S: Service<Call, Response = Exchange, Error = APIClientError>,
{
    let Call {
        req,
        permit,
        permit_wait,
    } = call;
    let audit_response_body = req.audit.as_ref().is_none_or(|a| a.audit_response_body);
    let log_prefix = req
        .audit
        .as_ref()
        .and_then(|a| a.name.as_deref())
        .map_or_else(|| "[audit]".to_string(), |n| format!("[audit:{n}]"));
    let cookie_header = cookie_header_for(&layer.cookies, &req.url);

    let mut timings = AuditTimings::default();
    // The store is shared with the request write task.
    let store = store.map(Arc::new);
    let request_write = record_request(
        &req,
        store.as_ref(),
        cookie_header.as_deref(),
        &log_prefix,
        &mut timings,
    );
    let mut ctx = Ctx {
        method: req.method,
        log_prefix,
        store,
        request_write,
        timings,
        hold: req.hold_permit,
        tee: req.audit.as_ref().is_some_and(|a| a.tee_response),
        body_limit: layer.body_limit,
        tracker: layer.tracker,
    };

    let called = Instant::now();
    let exchange = inner
        .call(Call {
            req,
            permit,
            permit_wait,
        })
        .await;
    ctx.timings.head_ms = ms(called.elapsed());

    let Exchange {
        resp,
        permit,
        audit: _,
    } = match exchange {
        Ok(exchange) => exchange,
        Err(err) => {
            // Without the URL's query: search parameters can carry personal
            // codes.
            error!("{} error response: {}", ctx.log_prefix, err.redacted());
            ctx.finish_request_write().await;
            return Err(err);
        }
    };

    let buffered =
        audit_response_body && resp.content_length().is_some_and(|n| n <= ctx.body_limit);
    if buffered {
        ctx.buffered(resp, permit).await
    } else {
        // No body is read here, so the permit ends at the head unless it is
        // held until the body does.
        let permit = if ctx.hold { permit } else { None };
        if !audit_response_body {
            if ctx.store.is_none() {
                debug!(
                    "{} response: {} (body muted)",
                    ctx.log_prefix,
                    resp.status()
                );
            }
            ctx.finish_request_write().await;
            Ok(ctx.exchange(resp, permit))
        } else if ctx.tee && ctx.store.is_some() {
            ctx.teed(resp, permit).await
        } else {
            ctx.head_only(resp, permit).await
        }
    }
}

impl Ctx {
    fn exchange(self, resp: reqwest::Response, permit: Option<OwnedSemaphorePermit>) -> Exchange {
        Exchange {
            resp,
            permit,
            audit: Some(self.timings),
        }
    }

    /// Read the body whole and audit it as received.
    async fn buffered(
        mut self,
        resp: reqwest::Response,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Result<Exchange, APIClientError> {
        let status = resp.status();
        let version = resp.version();
        let headers = resp.headers().clone();
        let read = Instant::now();
        let body = match resp.bytes().await {
            Ok(body) => body,
            Err(err) => {
                self.finish_request_write().await;
                return Err(err.into());
            }
        };
        self.timings.body = Some(BodyAudit::Buffered);
        self.timings.body_ms = Some(ms(read.elapsed()));
        self.timings.response_bytes = Some(body.len() as u64);
        // The body is in memory: the exchange with the server is over, so the
        // permit goes before the (possibly slow) response record is written.
        drop(permit);

        if let Some(store) = &self.store {
            let path = store.meta.path(self.method, "response");
            self.timings.response_record_ms =
                Some(ms(store.write(&path, vec![body.clone()]).await));
        } else {
            debug!(
                "{} response: {status}\n{}",
                self.log_prefix,
                log_body(&body)
            );
        }
        self.finish_request_write().await;

        let mut builder = http::Response::builder().status(status).version(version);
        if let Some(h) = builder.headers_mut() {
            *h = headers;
        }
        let http_resp = builder.body(body)?;
        Ok(self.exchange(reqwest::Response::from(http_resp), None))
    }

    /// The body is left to the caller; only the head is audited.
    async fn head_only(
        mut self,
        resp: reqwest::Response,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Result<Exchange, APIClientError> {
        self.timings.body = Some(BodyAudit::HeadOnly);
        let note = not_audited_note(resp.content_length());
        if let Some(store) = &self.store {
            let record = head_record(resp.version(), resp.status(), resp.headers(), &note);
            let path = store.meta.path(self.method, "response");
            self.timings.response_record_ms =
                Some(ms(store.write(&path, vec![Bytes::from(record)]).await));
        } else {
            debug!(
                "{} response: {} [body not logged: {}]",
                self.log_prefix,
                resp.status(),
                note.trim_start_matches("[body not audited: ")
                    .trim_end_matches(']')
            );
        }
        self.finish_request_write().await;
        Ok(self.exchange(resp, permit))
    }

    /// The body is stored by a task of its own while the caller reads it.
    async fn teed(
        mut self,
        resp: reqwest::Response,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Result<Exchange, APIClientError> {
        self.timings.body = Some(BodyAudit::Tee);
        let Some(store) = self.store.clone() else {
            return Ok(self.exchange(resp, permit));
        };
        let (status, version, headers) = (resp.status(), resp.version(), resp.headers().clone());
        let headers_for_caller = headers.clone();
        let (streams, state) = tee(resp.bytes_stream().boxed());
        let path = store.meta.path(self.method, "response");
        let partial_path = store.meta.path(self.method, "response_partial");
        let guard = self.tracker.start();
        tokio::spawn(async move {
            let _guard = guard;
            // No write slot while the body streams: it can last as long as the
            // caller likes, and its memory is bounded by the channel.
            let _ = store
                .auditor
                .write_audit_stream(&path, streams.auditor)
                .await;
            if !state.done.load(Ordering::SeqCst) {
                let note = format!(
                    "[body incomplete: {} bytes seen]",
                    state.seen.load(Ordering::Relaxed)
                );
                let record = head_record(version, status, &headers, &note);
                let _ = store.write(&partial_path, vec![Bytes::from(record)]).await;
            }
        });
        // The record of the stream is not waited for: it ends with the body.
        let mut builder = http::Response::builder().status(status).version(version);
        if let Some(h) = builder.headers_mut() {
            *h = headers_for_caller;
        }
        self.finish_request_write().await;
        let http_resp = builder.body(reqwest::Body::wrap_stream(streams.caller))?;
        Ok(self.exchange(reqwest::Response::from(http_resp), permit))
    }

    /// Wait for the request record to be written.
    async fn finish_request_write(&mut self) {
        let Some(task) = self.request_write.take() else {
            return;
        };
        match task.await {
            Ok(took) => self.timings.request_record_ms = Some(ms(took)),
            Err(err) => warn!("audit request record task failed: {err}"),
        }
    }
}

/// Start writing the request record, or log the request when nothing is
/// stored. The write is a task of its own: it ends even when the caller's
/// future is dropped, and it overlaps the exchange.
fn record_request(
    req: &HttpRequest,
    store: Option<&Arc<Store>>,
    cookies: Option<&str>,
    log_prefix: &str,
    timings: &mut AuditTimings,
) -> Option<RequestWrite> {
    let Some(store) = store else {
        let curl = build_curl(req, cookies);
        timings.request_bytes = curl.len() as u64;
        debug!("{log_prefix} request: {curl}");
        return None;
    };
    let chunks = request_to_curl_chunks(
        req.url.as_str(),
        req.method,
        &req.headers,
        req.body.as_ref(),
        cookies,
    );
    timings.request_bytes = chunks.iter().map(|c| c.len() as u64).sum();
    let path = store.meta.path(req.method, "request");
    let store = store.clone();
    Some(tokio::spawn(
        async move { store.write(&path, chunks).await },
    ))
}
