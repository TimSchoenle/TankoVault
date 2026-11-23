//! Owns the fuzz target that exercises `tankovault_fetch`'s multi-member gzip decoder, reached
//! through the `#[doc(hidden)] __fuzz` seam.
//!
//! # Oracle
//! 1. No panic on arbitrary compressed bytes.
//! 2. A decoded body never exceeds the 16 MiB body cap: a gzip bomb must be refused, not inflated.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(body) = tankovault_fetch::__fuzz::decode_gzip(data) {
        assert!(
            body.len() <= tankovault_solver::MAX_BODY_BYTES,
            "inflated past the body cap: {} bytes",
            body.len()
        );
    }
});
