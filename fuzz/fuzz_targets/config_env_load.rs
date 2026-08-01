//! **F-T6** — `tankovault_config::load`, the layered `TANKOVAULT_*` environment reader every
//! service boots through.
//!
//! Its input is a deployment's environment, which is not attacker-controlled in the way a
//! provider's HTML is — so the threat model here is a **typo**, not an adversary. That is worth
//! fuzzing for one specific reason: figment *ignores* an unknown `TANKOVAULT_*` key rather than
//! rejecting it, so a misspelling is already a silent no-op (this is what `docs/CONFIGURATION.md`
//! §8 and the `config-docs` gate exist for). The failure this target is looking for is the worse
//! one — a key that is spelled *right* and whose value makes a service abort at startup instead
//! of reporting a typed error, which reads to an operator as "the new release is broken".
//!
//! # Oracle
//!
//! 1. **`Ok` or `ConfigError`, never a panic** — through the extraction *and* through the
//!    post-parse accessors, which are where the second round of interpretation happens and
//!    where the audit pointed: `InternalAuthConfig::resolve` and `EmailConfig::effective_port`.
//! 2. **The internal-token length floor holds over a value that came through env parsing.**
//!    `resolve` is the only startup check standing between a deployment and a guessable
//!    service-to-service secret (SEC-1), and it trims before it measures, so the length it
//!    reports and the length it checked have to be the same one.
//! 3. **The feature-flag refresh interval is at least a second.** `refresh_secs` is a `u64`
//!    read straight from the environment and `refresh_interval` clamps it, documented as
//!    stopping a misconfigured `0` from turning the refresh loop into a busy spin against the
//!    database. A clamp is exactly the kind of guarantee that survives review and not
//!    refactoring.
//!
//! # Input shape
//!
//! One `KEY=VALUE` per line, with the `TANKOVAULT_` prefix supplied by this target rather than
//! by the mutator. Two deliberate choices:
//!
//! - **Text, not `arbitrary::Arbitrary` over `Vec<(String, String)>`.** The audit sketched the
//!   structured form; the cost is that every seed becomes an opaque byte blob, and `seeds/` in
//!   this tree is committed, readable and distilled. `KEY=VALUE` lines are structured enough to
//!   keep the mutator on the interesting axis and still `cat`-able.
//! - **The prefix is not fuzzed.** `Env::prefixed("TANKOVAULT_")` discards everything without
//!   it, so bytes spent generating one are bytes spent reaching a no-op.
//!
//! `TANKOVAULT_CONFIG` is filtered out of the fuzzer's reach: `load` reads it as a path to a
//! TOML file, and a reproducer whose behaviour depends on a file elsewhere on the machine is
//! not a reproducer. The jail's empty working directory is what the loader sees instead.

#![no_main]

use figment::Jail;
use libfuzzer_sys::fuzz_target;
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use tankovault_config::{
    AuditConfig, DatabaseConfig, EmailConfig, FeaturesConfig, InternalAuthConfig,
    MIN_INTERNAL_TOKEN_LEN, MatchingConfig, MetricsConfig, NatsConfig, RateLimitConfig,
    RedisConfig, SecurityConfig, TelemetryConfig, is_production,
};
use tankovault_domain::matching::{Candidate, Canonicaliser, Query};
use tankovault_domain::{ContentType, SeriesId};

/// The union of every configuration block `crates/config` publishes.
///
/// Deliberately **not** a copy of any one service's `Config`: those are private structs inside
/// binary crates, so this cannot reach them, and a hand-mirrored copy of one would be a second
/// artefact to keep in step for no gain. Composing the union instead means the only way this
/// drifts is by a *new* block being added to `crates/config` and not listed here — which costs
/// coverage of that block and cannot produce a wrong answer about the others.
///
/// `database` and `telemetry` are the two blocks with no serde default (`url`, `service_name`),
/// which is why [`BASE_ENV`] supplies them.
#[derive(Debug, Deserialize)]
#[expect(
    dead_code,
    reason = "the deserializer is the consumer. Half of these blocks are never read back by \
              the oracle, and reading them would prove nothing the extraction has not already \
              proven — the parse *is* the code under test. Dropping the unread ones would \
              shrink the surface this target covers, which is the opposite of the point."
)]
struct UnionConfig {
    database: DatabaseConfig,
    telemetry: TelemetryConfig,
    #[serde(default)]
    nats: Option<NatsConfig>,
    #[serde(default)]
    redis: Option<RedisConfig>,
    #[serde(default)]
    email: EmailConfig,
    #[serde(default)]
    security: SecurityConfig,
    #[serde(default)]
    rate_limit: RateLimitConfig,
    #[serde(default)]
    metrics: MetricsConfig,
    #[serde(default)]
    audit: AuditConfig,
    #[serde(default)]
    features: FeaturesConfig,
    #[serde(default)]
    internal: InternalAuthConfig,
    #[serde(default)]
    matching: MatchingConfig,
}

/// A minimally bootable environment, applied before the fuzzer's pairs so it can override any
/// of it.
///
/// Without this every input that does not happen to spell `DATABASE__URL` fails extraction on
/// the first missing required field, and the mutator never reaches the value parsing this
/// target is about — the whole run would measure how long it takes to guess a key name.
const BASE_ENV: &[(&str, &str)] = &[
    ("TANKOVAULT_DATABASE__URL", "postgres://u:p@localhost/tv"),
    ("TANKOVAULT_TELEMETRY__SERVICE_NAME", "fuzz"),
];

/// The most bytes of environment one iteration will build. A deployment with thousands of
/// `TANKOVAULT_*` variables is not a case worth spending fuzzing time on, and an unbounded
/// input here turns into an unbounded `set_env` loop.
const MAX_PAIRS: usize = 64;

fuzz_target!(|data: &str| {
    // Ignored: `Jail::try_with` reports a jail-setup failure (a temp dir it could not create),
    // which says nothing about the code under test. Every outcome that *is* about the code is
    // handled inside the closure.
    let _ = Jail::try_with(|jail| {
        for (key, value) in BASE_ENV {
            jail.set_env(key, value);
        }

        for line in data.lines().take(MAX_PAIRS) {
            let Some((suffix, value)) = line.split_once('=') else {
                continue;
            };
            // `std::env::set_var` panics on a key containing `=` or NUL, or a value containing
            // NUL — preconditions of the platform API, not behaviour of the loader. Splitting on
            // the first `=` already excludes it from the key; NUL has to be excluded by hand.
            // The key is never empty because the prefix is always there.
            if suffix.contains('\0') || value.contains('\0') {
                continue;
            }
            let key = format!("TANKOVAULT_{suffix}");
            // See the module doc: the TOML half is deliberately out of reach.
            if key == "TANKOVAULT_CONFIG" {
                continue;
            }
            jail.set_env(&key, value);
        }

        // (1) The extraction itself. A `ConfigError` is a legitimate outcome for almost any
        // input here — that *is* the contract — so the result is inspected rather than asserted.
        let Ok(cfg) = tankovault_config::load::<UnionConfig>() else {
            return Ok(());
        };

        // (2) The internal-token floor, over a value that survived env parsing.
        let production = is_production();
        if let Ok(Some(token)) = cfg.internal.resolve(production) {
            // `resolve` hands back a `SecretString`, so the oracle exposes it here. That is
            // the right shape for a fuzz target: the assertions are about the token's
            // *length* and *trimming*, and neither message can print the value.
            let token = token.expose_secret();
            assert!(
                token.len() >= MIN_INTERNAL_TOKEN_LEN,
                "resolve() returned a {}-character internal token, under the {MIN_INTERNAL_TOKEN_LEN}-character floor",
                token.len()
            );
            assert_eq!(token, token.trim(), "resolve() returned an untrimmed token");
        }

        // (3) The refresh clamp.
        assert!(
            cfg.features.refresh_interval() >= std::time::Duration::from_secs(1),
            "refresh_interval() fell below the one-second clamp at refresh_secs={}",
            cfg.features.refresh_secs
        );

        // (1), continued: the accessors that interpret what was parsed. No invariant is
        // asserted over these — the port default that `effective_port` applies is a policy
        // choice, not a bound — so what is being claimed is totality.
        let _ = cfg.email.effective_port();
        let _ = cfg.email.effective_envelope_from();
        let _ = cfg.email.is_enabled();
        let _ = cfg.audit.retention_enabled();
        let _ = cfg.security.cors.is_enabled();

        // The one place a configured **float** decides something. `high`/`low` are `f32` read
        // from the environment, so `TANKOVAULT_MATCHING__HIGH=nan` is a reachable spelling and
        // every band comparison against it is false. Totality only: which side of the band a
        // NaN threshold lands on is a policy question rather than an invariant to invent here.
        let query = Query {
            normalized_title: "one punch man".to_owned(),
            content_type: ContentType::Manga,
            release_year: Some(2012),
            tags: vec!["action".to_owned()],
            authors: vec!["one".to_owned()],
        };
        let candidate = Candidate {
            series_id: SeriesId::new(),
            normalized_title: "one punch man".to_owned(),
            // Added when `Candidate` grew the field; this literal had not been updated, so
            // the fuzz workspace did not compile. It is excluded from the host workspace and
            // from every CI gate, which is exactly why the drift went unnoticed.
            alt_normalized_titles: Vec::new(),
            similarity: 1.0,
            content_type: ContentType::Manga,
            release_year: Some(2012),
            tags: vec!["action".to_owned()],
            authors: vec!["one".to_owned()],
        };
        let _ = cfg.matching.candidate_limit();
        let _ = cfg
            .matching
            .canonicalise(&query, std::slice::from_ref(&candidate));

        Ok(())
    });
});
