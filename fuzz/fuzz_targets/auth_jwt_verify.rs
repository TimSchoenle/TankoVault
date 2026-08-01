//! **F-T5** — `tankovault_auth::verify_access_token`, which is the whole of the API's
//! authentication.
//!
//! `crates/db` never sees an access token, so there is no second check behind this one: a
//! string that gets past it is a principal. `jsonwebtoken` is the actual parser, so a crash
//! here is unlikely — that is not the reason this target exists. It exists because the two
//! things this repository adds *around* that parser are both attacker-reachable and neither is
//! covered by the library's own test suite: the pinned algorithm, and `AccessClaims::user_id`
//! calling `Uuid::from_str` on a `sub` the token's holder chose.
//!
//! # Oracle
//!
//! Four, and only the first is the usual "no panic":
//!
//! 1. **No panic**, over arbitrary UTF-8.
//! 2. **A token that verifies carries `alg: HS256` in its own header.** `verify_access_token`
//!    pins the algorithm rather than reading it from the token, which is the defence against
//!    the classic JWT confusion attack: swap `alg` for `none` and drop the signature, or swap
//!    it for `RS256` so the HMAC secret is treated as an RSA public key and the holder signs
//!    their own claims. This is the assertion that says the pin is real, and it is checked
//!    with `decode_header` — the *same* parser the verifier uses — so a disagreement between
//!    what was validated and what the header says is what fails, rather than a second opinion
//!    from a hand-rolled base64 decoder.
//! 3. **A token that verifies under one secret does not verify under another.** A forgery
//!    would have to break HMAC-SHA256 to reach this, and it is one comparison, so it costs
//!    nothing to leave standing. What it genuinely guards is a future change that widens the
//!    key handling — a `DecodingKey` built from something that is not the secret, or an empty
//!    secret treated as "no verification".
//! 4. **`user_id()` is total.** `sub` is a plain `String` in the claims, so a validly-signed
//!    token whose subject is not a UUID verifies and then yields no user. That is why the
//!    method returns `Option` and why every caller has to treat an unparseable subject as
//!    "no principal"; calling it here is what puts `Uuid::from_str` in the fuzzer's reach.
//!
//! # Seeds
//!
//! `seeds/auth_jwt_verify/` holds tokens minted against the secret below, mutated in the ways
//! that matter (algorithm swapped, signature stripped, payload edited without re-signing, `sub`
//! made non-UUID). They are reproducible: each is `base64url(header).base64url(payload).
//! base64url(HMAC-SHA256(secret, header.payload))`, and their `exp` is deliberately far in the
//! future so the "this one verifies" branch does not silently stop being reachable when the
//! clock passes an expiry baked into a committed file. A seed whose `exp` had passed would
//! still be a legal input — it would simply exercise the rejection path that a dozen other
//! seeds already cover, and nothing would say so.

#![no_main]

use jsonwebtoken::{Algorithm, decode_header};
use libfuzzer_sys::fuzz_target;
use secrecy::SecretSlice;
use std::sync::LazyLock;
use tankovault_auth::verify_access_token;

/// The secret the committed seeds are signed with. Named in the audit's own sketch of this
/// target; it is a fuzzing constant and seals nothing.
///
/// A `LazyLock` rather than a `const` because `verify_access_token` takes a
/// [`SecretSlice<u8>`], which owns a heap allocation and so cannot be a constant. Built once
/// per process, not once per iteration — a fresh allocation on every input would be pure
/// overhead in the hot loop the fuzzer spends all its time in.
static SECRET: LazyLock<SecretSlice<u8>> =
    LazyLock::new(|| SecretSlice::from(b"fuzz-secret-please-rotate".to_vec()));

/// Any other key. Only its difference from [`SECRET`] matters.
static OTHER_SECRET: LazyLock<SecretSlice<u8>> =
    LazyLock::new(|| SecretSlice::from(b"a-different-secret-entirely".to_vec()));

fuzz_target!(|data: &str| {
    let Ok(claims) = verify_access_token(&SECRET, data) else {
        // The overwhelmingly common outcome, and still worth reaching: it is the path that
        // walks the token's own header and signature, which is where a parser crash would be.
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

    // (4) `Uuid::from_str` over an attacker-chosen subject. The result is deliberately not
    // asserted — a non-UUID `sub` yielding `None` is the documented, correct answer, and the
    // claim being made here is only that asking cannot abort.
    let _ = claims.user_id();
});
