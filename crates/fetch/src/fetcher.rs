//! The `Fetcher` trait — the transport contract adapters depend on (they never own a
//! transport; it is injected, so it is testable and swappable — design Appendix A §5).

use crate::error::FetchError;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;

/// A single GET through the (possibly decorated) fetch stack.
#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Perform the request, returning a bounded-text response or a typed error.
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError>;
}

#[async_trait]
impl<T: Fetcher + ?Sized> Fetcher for std::sync::Arc<T> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        (**self).get(req).await
    }
}
