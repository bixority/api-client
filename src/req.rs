use crate::types::{APIClientError, Call, Exchange, RequestTimeout};
use futures::future::BoxFuture;
use std::task::{Context, Poll};
use std::time::Duration;
use tower::Service;

#[derive(Clone)]
pub struct ReqwestService {
    client: reqwest::Client,
    default_timeout: Option<Duration>,
}

impl ReqwestService {
    pub(crate) const fn new(client: reqwest::Client, default_timeout: Option<Duration>) -> Self {
        Self {
            client,
            default_timeout,
        }
    }
}

impl Service<Call> for ReqwestService {
    type Response = Exchange;
    type Error = APIClientError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, call: Call) -> Self::Future {
        let client = self.client.clone();
        let default_timeout = self.default_timeout;

        Box::pin(async move {
            let Call { req, permit, .. } = call;
            let mut r = client
                .request(req.method.into_reqwest(), req.url)
                .headers(req.headers);

            let timeout = match req.timeout {
                RequestTimeout::Default => default_timeout,
                RequestTimeout::After(d) => Some(d),
                RequestTimeout::None => None,
            };
            if let Some(timeout) = timeout {
                r = r.timeout(timeout);
            }

            if let Some(body) = req.body {
                r = r.body(body);
            }

            let resp = r.send().await?;
            Ok(Exchange {
                resp,
                permit,
                audit: None,
            })
        })
    }
}
