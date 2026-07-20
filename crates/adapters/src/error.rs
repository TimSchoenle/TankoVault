//! Adapter error type.

use tankovault_domain::ResolveError;
use tankovault_fetch::FetchError;

/// Errors raised while enumerating or parsing a provider.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// A fetch through the injected stack failed.
    #[error(transparent)]
    Fetch(#[from] FetchError),
    /// A relative path could not be resolved against the provider base URL.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// The provider returned a non-success status for a page.
    #[error("provider returned HTTP {0}")]
    Http(u16),
    /// A CSS selector in the adapter config was invalid.
    #[error("invalid selector {selector:?}: {reason}")]
    Selector { selector: String, reason: String },
    /// The adapter config JSON did not match the expected schema.
    #[error("invalid adapter config: {0}")]
    Config(String),
    /// A required element was absent from the parsed page.
    #[error("required element not found: {0}")]
    Missing(String),
    /// No adapter is registered for a `custom` provider slug.
    #[error("no custom adapter registered for provider {0:?}")]
    UnknownCustom(String),
}
