//! Owns the fuzz target that exercises the Astro island `props` decoder (`astro::unwrap_astro`),
//! reached through the `#[doc(hidden)] __fuzz` seam.
//!
//! # Oracle
//! No panic and no stack overflow: the props are provider-controlled, and the decoder recurses
//! over them under a depth limit.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    let _ = tankovault_adapters::__fuzz::astro_props(data);
});
