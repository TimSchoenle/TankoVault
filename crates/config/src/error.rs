//! The one error type configuration loading can produce.

/// Errors raised while assembling configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A figment extraction/merge failure (missing required key, type mismatch, ...).
    /// Boxed because `figment::Error` is large relative to a typical `Result`.
    #[error("configuration error: {0}")]
    Figment(#[from] Box<figment::Error>),
    /// A value parsed but is not usable — too short, or absent where the active profile
    /// requires it. Kept distinct from [`Self::Figment`] so the message can name the fix.
    #[error("configuration error: {0}")]
    Invalid(String),
}
