//! Defines [`Fetcher`], the transport contract adapters depend on.
//!
//! Adapters never own a transport; it is injected, so it is testable and swappable.

use crate::error::FetchError;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;

/// Sends a single GET through the fetch stack, decorated or not.
#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Performs the request, returning a bounded-text response or a typed error.
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError>;
}

#[async_trait]
impl<T: Fetcher + ?Sized> Fetcher for std::sync::Arc<T> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        (**self).get(req).await
    }
}
