//! Defines [`AuthError`].

/// Failures from hashing, token issuance and validation, and sealing.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Password hashing failed.
    #[error("password hashing failed")]
    Hashing,
    /// A stored password hash could not be parsed.
    #[error("stored password hash is malformed")]
    MalformedHash,
    /// JWT encoding failed.
    #[error("failed to issue token")]
    TokenIssue,
    /// A JWT was invalid, expired or tampered with.
    #[error("invalid or expired token")]
    InvalidToken,
    /// A data-encryption key was not valid base64 or not exactly 32 bytes.
    #[error("invalid encryption key")]
    InvalidKey,
    /// Authenticated encryption or decryption failed: a wrong key, tampering or truncation.
    #[error("cryptographic operation failed")]
    Crypto,
}
