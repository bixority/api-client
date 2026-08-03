use crate::types::APIClientError;
use futures::future::BoxFuture;

pub trait Auditor: Send + Sync {
    fn write_audit_data<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), APIClientError>>;
}
