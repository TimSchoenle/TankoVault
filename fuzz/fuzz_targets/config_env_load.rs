//! Fuzzes `tankovault_config::load`, the layered `TANKOVAULT_*` environment reader every
//! service boots through. The environment isn't attacker-controlled the way provider HTML is,
//! so the threat model is a typo: figment ignores an unknown key rather than rejecting it, so
//! this target looks for the worse case — a correctly-spelled key whose value aborts a service
//! at startup instead of producing a typed error.
//!
//! # Oracle
//! 1. `Ok` or `ConfigError`, never a panic — through extraction and the post-parse accessors
//!    (`InternalAuthConfig::resolve`, `EmailConfig::effective_port`) where a second round of
//!    interpretation happens.
//! 2. The internal-token length floor holds over a value that came through env parsing:
//!    `resolve` trims before it measures, so the length it reports and the length it checked
//!    must be the same one.
//! 3. The feature-flag refresh interval is at least a second: `refresh_interval` clamps a `u64`
//!    read straight from the environment, so a misconfigured `0` can't turn the refresh loop
//!    into a busy spin.
//!
//! # Input shape
//! One `KEY=VALUE` per line, with the `TANKOVAULT_` prefix supplied by this target so seeds
//! stay readable text rather than an opaque byte blob, and so mutation time isn't spent
//! generating a prefix that `Env::prefixed` just discards.
//!
//! The keys that make `load` read the filesystem are filtered out — `TANKOVAULT_CONFIG` (a TOML
//! file or directory), `TANKOVAULT_SECRETS_DIR`, and anything ending `_FILE` (a path holding one
//! value). A reproducer whose behaviour depends on a file elsewhere on the machine isn't a
//! reproducer. The value *parsing* those layers feed is the same code this target already
//! covers through the environment; what they add is I/O, which belongs in the unit tests in
//! `crates/config/src/secrets.rs`.

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
/// Not a copy of any one service's `Config` (those are private structs in binary crates):
/// composing the union instead means drift only costs coverage of a newly-added block, never a
/// wrong answer about the others.
///
/// `database` and `telemetry` have no serde default (`url`, `service_name`), which is why
/// [`BASE_ENV`] supplies them.
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
/// Without this, most inputs fail extraction on the first missing required field before ever
/// reaching the value parsing this target is about.
const BASE_ENV: &[(&str, &str)] = &[
    ("TANKOVAULT_DATABASE__URL", "postgres://u:p@localhost/tv"),
    ("TANKOVAULT_TELEMETRY__SERVICE_NAME", "fuzz"),
];

/// The most bytes of environment one iteration will build — an unbounded input here becomes
/// an unbounded `set_env` loop.
const MAX_PAIRS: usize = 64;

fuzz_target!(|data: &str| {
    // Ignored: a jail-setup failure (e.g. a temp dir it couldn't create) says nothing about
    // the code under test; every outcome that does is handled inside the closure.
    let _ = Jail::try_with(|jail| {
        for (key, value) in BASE_ENV {
            jail.set_env(key, value);
        }

        for line in data.lines().take(MAX_PAIRS) {
            let Some((suffix, value)) = line.split_once('=') else {
                continue;
            };
            // `set_var` panics on a key/value containing NUL (a platform precondition, not
            // loader behaviour); `=` is already excluded by the split above.
            if suffix.contains('\0') || value.contains('\0') {
                continue;
            }
            let key = format!("TANKOVAULT_{suffix}");
            // See the module doc: every layer that reads the filesystem is deliberately out of
            // reach, so a crash always reproduces from the input alone.
            if key == "TANKOVAULT_CONFIG"
                || key == "TANKOVAULT_SECRETS_DIR"
                || key.ends_with("_FILE")
            {
                continue;
            }
            jail.set_env(&key, value);
        }

        // (1) The extraction itself; a `ConfigError` is a legitimate outcome for almost any input.
        let Ok(cfg) = tankovault_config::load::<UnionConfig>() else {
            return Ok(());
        };

        // (2) The internal-token floor, over a value that survived env parsing.
        let production = is_production();
        if let Ok(Some(token)) = cfg.internal.resolve(production) {
            // `resolve` hands back a `SecretString`; exposed here since the assertions are
            // about length and trimming, and neither message can print the value.
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

        // (1) continued: accessors that interpret what was parsed. No invariant asserted here
        // — the port default `effective_port` applies is policy, not a bound — only totality.
        let _ = cfg.email.effective_port();
        let _ = cfg.email.effective_envelope_from();
        let _ = cfg.email.is_enabled();
        let _ = cfg.audit.retention_enabled();
        let _ = cfg.security.cors.is_enabled();

        // The one place a configured float decides something: `high`/`low` are `f32` read from
        // the environment, so a NaN threshold is a reachable spelling and every band comparison
        // against it is false. Totality only.
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
            // Added when `Candidate` grew this field; excluded from the host workspace and
            // every CI gate, which is why this literal silently went stale until it failed to
            // build.
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
