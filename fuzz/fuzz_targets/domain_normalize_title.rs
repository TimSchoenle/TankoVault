//! Owns the fuzz target that exercises `tankovault_domain::normalize_title`, the matcher's
//! title key, over provider-supplied titles.
//!
//! # Oracle
//! 1. No panic: lowercasing can expand a character, the classic byte-offset trap.
//! 2. Idempotent, with no leading, trailing or doubled space: a key that changes when
//!    re-normalised would orphan rows from their own matching key.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tankovault_domain::normalize_title;

fuzz_target!(|data: &str| {
    let key = normalize_title(data);
    assert_eq!(normalize_title(&key), key, "not idempotent for {data:?}");
    assert!(!key.starts_with(' ') && !key.ends_with(' '), "untrimmed {key:?}");
    assert!(!key.contains("  "), "doubled space in {key:?}");
});
