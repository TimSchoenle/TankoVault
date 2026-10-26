//! Argon2id, run off the async runtime.
//!
//! One hash is tens of milliseconds of CPU over 19 MiB. Run inline it occupies a Tokio worker for
//! that long, so a handful of concurrent sign-ins stalled every other request scheduled on those
//! workers — SSE streams and `/ready` included. Each call here takes a permit from a pool sized to
//! the CPU count and runs on the blocking pool; a caller that cannot get a permit within
//! [`QUEUE_WAIT`] is answered `503` rather than queued without bound.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
use tokio::sync::Semaphore;

use crate::error::{ApiError, ApiResult};

/// How long a request waits for a hashing slot before it is shed.
const QUEUE_WAIT: Duration = Duration::from_secs(5);

/// One permit per core: more concurrent hashes than cores only adds memory, not throughput.
static PERMITS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    Arc::new(Semaphore::new(cores))
});

/// Run `work` on the blocking pool under a hashing permit.
///
/// The permit moves into the closure, so a request cancelled mid-hash still holds its slot until
/// the CPU work actually ends — otherwise abandoned hashes would oversubscribe the pool.
async fn offload<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    let permit = tokio::time::timeout(QUEUE_WAIT, Arc::clone(&PERMITS).acquire_owned())
        .await
        .map_err(|_| {
            tracing::warn!("password hashing saturated; shedding the request");
            ApiError::Unavailable
        })?
        .map_err(|_| ApiError::Internal)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| ApiError::Internal)
}

/// [`tankovault_auth::hash_password`], off the runtime.
///
/// # Errors
/// [`ApiError::Unavailable`] when no slot frees up in time; [`ApiError::Internal`] when hashing
/// fails.
pub(crate) async fn hash_password(
    password: &SecretString,
    pepper: &Arc<SecretSlice<u8>>,
) -> ApiResult<String> {
    let password = SecretString::from(password.expose_secret());
    let pepper = Arc::clone(pepper);
    offload(move || tankovault_auth::hash_password(&password, &pepper))
        .await?
        .map_err(|_| ApiError::Internal)
}

/// [`tankovault_auth::verify_password`], off the runtime.
///
/// # Errors
/// As [`hash_password`]; an unparseable stored hash is [`ApiError::Internal`].
pub(crate) async fn verify_password(
    password: &SecretString,
    hash: &str,
    pepper: &Arc<SecretSlice<u8>>,
) -> ApiResult<bool> {
    let password = SecretString::from(password.expose_secret());
    let hash = hash.to_owned();
    let pepper = Arc::clone(pepper);
    offload(move || tankovault_auth::verify_password(&password, &hash, &pepper))
        .await?
        .map_err(|_| ApiError::Internal)
}

#[cfg(test)]
mod tests {
    use super::{hash_password, verify_password};
    use secrecy::{SecretSlice, SecretString};
    use std::sync::Arc;

    #[tokio::test]
    async fn an_offloaded_hash_verifies_and_rejects_like_the_inline_one() {
        let pepper = Arc::new(SecretSlice::from(b"pepper".to_vec()));
        let password = SecretString::from("correct horse battery staple");
        let hash = hash_password(&password, &pepper).await.expect("hashes");
        assert!(
            verify_password(&password, &hash, &pepper)
                .await
                .expect("verifies")
        );
        assert!(
            !verify_password(&SecretString::from("guess"), &hash, &pepper)
                .await
                .expect("verifies")
        );
    }
}
