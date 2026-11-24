//! Owns the response-body size cap shared by every client that buffers a solver or upstream body.

use futures::StreamExt as _;

use crate::SolveError;

/// The hard cap on a buffered response body; `tankovault-fetch` applies the same one.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Buffers `resp`'s body, refusing one past [`MAX_BODY_BYTES`] instead of growing without bound.
///
/// # Errors
/// [`SolveError::Malformed`] if the body is oversized or the stream breaks.
pub async fn read_capped(resp: wreq::Response) -> Result<Vec<u8>, SolveError> {
    // `Content-Length` only pre-sizes, clamped: the loop below is the bound.
    let declared = resp
        .content_length()
        .and_then(|n| usize::try_from(n).ok())
        .map_or(0, |n| n.min(MAX_BODY_BYTES));
    let mut buf = Vec::with_capacity(declared);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| SolveError::Malformed(e.to_string()))?;
        if buf.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(SolveError::Malformed(format!(
                "response body exceeds {MAX_BODY_BYTES} bytes"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}
