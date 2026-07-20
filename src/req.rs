use crate::types::{APIClientError, HttpRequest};
use futures::future::BoxFuture;
use std::task::{Context, Poll};
use tower::Service;

#[derive(Clone)]
pub struct ReqwestService {
    client: reqwest::Client,
}

impl ReqwestService {
    pub(crate) const fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Service<HttpRequest> for ReqwestService {
    type Response = reqwest::Response;
    type Error = APIClientError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        let client = self.client.clone();

        Box::pin(async move {
            let mut r = client
                .request(req.method.into_reqwest(), req.url)
                .headers(req.headers);

            if let Some(body) = req.body {
                r = r.body(body);
            }

            Ok(r.send().await?)
        })
    }
}
