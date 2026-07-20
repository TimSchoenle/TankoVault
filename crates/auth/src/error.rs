//! Auth error type.

/// Failures from hashing/verification, token issuance/validation, and secret sealing.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Password hashing failed.
    #[error("password hashing failed")]
    Hashing,
    /// A stored password hash could not be parsed (corruption/migration).
    #[error("stored password hash is malformed")]
    MalformedHash,
    /// JWT encoding failed.
    #[error("failed to issue token")]
    TokenIssue,
    /// JWT was invalid, expired, or tampered with.
    #[error("invalid or expired token")]
    InvalidToken,
    /// A data-encryption key was not valid base64 or not exactly 32 bytes.
    #[error("invalid encryption key")]
    InvalidKey,
    /// Authenticated encryption/decryption failed (wrong key, tampering, truncation).
    #[error("cryptographic operation failed")]
    Crypto,
}
