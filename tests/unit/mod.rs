#![allow(deprecated)]
// `HttpResponse` and `APIClient` carry a permit type with a significant `Drop`.
#![allow(clippy::significant_drop_tightening)]

#[cfg(feature = "audit-core")]
mod audit_layer_tests;
#[cfg(feature = "audit-core")]
mod audit_tests;
#[cfg(feature = "audit-core")]
mod background_tests;
mod client_tests;
mod cookie_tests;
#[cfg(feature = "audit")]
mod object_storage_test;
mod request_tests;
mod retry_tests;
mod support;
mod transport_tests;
mod types_tests;
