//! Fuzzes `tankovault_auth::verify_access_token` — the whole of the API's authentication, with
//! no second check behind it: a string that gets past it is a principal. `jsonwebtoken` parses
//! the token; what this target covers is what this repo adds around it — the pinned algorithm,
//! and `AccessClaims::user_id`'s `Uuid::from_str` on an attacker-chosen `sub`.
//!
//! # Oracle
//! 1. No panic, over arbitrary UTF-8.
//! 2. A token that verifies carries `alg: HS256` in its own header — the defence against JWT
//!    algorithm-confusion (swap `alg` for `none` or `RS256` to sign your own claims), checked
//!    via `decode_header` so a disagreement between what verified and what the header says
//!    fails here.
//! 3. A token that verifies under one secret does not verify under another — cheap to assert,
//!    and guards a future change that widens key handling (e.g. an empty secret treated as
//!    "no verification").
//! 4. `user_id()` is total: a validly-signed token with a non-UUID `sub` yields `None`, not a
//!    panic, which is why every caller must treat an unparseable subject as no principal.
//!
//! # Seeds
//! `seeds/auth_jwt_verify/` holds tokens signed against the secret below, mutated the ways that
//! matter (algorithm swapped, signature stripped, payload edited unsigned, non-UUID `sub`).
//! Their `exp` is far in the future so the "verifies" branch stays reachable as the clock moves.

#![no_main]

use jsonwebtoken::{Algorithm, decode_header};
use libfuzzer_sys::fuzz_target;
use secrecy::SecretSlice;
use std::sync::LazyLock;
use tankovault_auth::verify_access_token;

/// The secret the committed seeds are signed with; a fuzzing constant that seals nothing.
///
/// `LazyLock`, not `const`: `verify_access_token` takes a [`SecretSlice<u8>`], which owns a
/// heap allocation. Built once per process, not per iteration, to keep it out of the hot loop.
static SECRET: LazyLock<SecretSlice<u8>> =
    LazyLock::new(|| SecretSlice::from(b"fuzz-secret-please-rotate".to_vec()));

/// Any other key. Only its difference from [`SECRET`] matters.
static OTHER_SECRET: LazyLock<SecretSlice<u8>> =
    LazyLock::new(|| SecretSlice::from(b"a-different-secret-entirely".to_vec()));

fuzz_target!(|data: &str| {
    let Ok(claims) = verify_access_token(&SECRET, data) else {
        // The common outcome, and still worth reaching: it walks the header/signature parse.
        return;
    };

    // (2) The algorithm is pinned, not read from the token.
    match decode_header(data) {
        Ok(header) => assert_eq!(
            header.alg,
            Algorithm::HS256,
            "a token verified while its own header declared {:?}",
            header.alg
        ),
        Err(e) => panic!("a token verified but its header does not parse: {e}"),
    }

    // (3) The signature is over this secret and no other.
    assert!(
        verify_access_token(&OTHER_SECRET, data).is_err(),
        "a token verified under two different secrets"
    );

    // (4) `Uuid::from_str` over an attacker-chosen subject; unasserted since a non-UUID `sub`
    // yielding `None` is correct — only that asking cannot abort is being claimed.
    let _ = claims.user_id();
});
