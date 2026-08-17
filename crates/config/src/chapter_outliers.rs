//! Operator control over which scraped chapter numbers ingest refuses.

use crate::ConfigError;
use serde::Deserialize;
use tankovault_domain::chapter_outliers::OutlierPolicy;

/// How aggressively a scan rejects chapter numbers a source cannot plausibly have released.
///
/// Every knob is relative to the listing's own spacing, so one setting covers a 20-chapter
/// series and a 4,000-chapter one. Raising [`Self::sparse_factor`] rejects less; lowering it
/// starts taking renumbered arcs with the junk. See
/// [`tankovault_domain::chapter_outliers`] for what each threshold measures.
///
/// ```
/// use tankovault_config::ChapterOutlierConfig;
/// use tankovault_domain::chapter_outliers::implausible_indices;
///
/// // A stray far above a contiguous run — the shape these sites actually publish.
/// let mut listing: Vec<f64> = (1..=40).map(f64::from).collect();
/// listing.push(9000.0);
///
/// let policy = ChapterOutlierConfig::default().policy();
/// assert_eq!(implausible_indices(&listing, &policy), [40]);
///
/// // Disabled by configuration: nothing is ever rejected, whatever the numbers look like.
/// let off = ChapterOutlierConfig { enabled: false, ..ChapterOutlierConfig::default() };
/// assert!(implausible_indices(&listing, &off.policy()).is_empty());
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct ChapterOutlierConfig {
    /// Whether a scan rejects anything at all.
    ///
    /// An escape hatch for the case the rule is wrong about a provider and dropping the
    /// chapters is worse than indexing the junk. Turning it off does not restore chapters
    /// already skipped — the next scan re-ingests them, since ingest is idempotent.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Smallest listing worth judging.
    #[serde(default = "default_min_sample")]
    pub min_sample: usize,
    /// Chapters that must survive any single scan.
    #[serde(default = "default_min_body")]
    pub min_body: usize,
    /// Absolute floor on a suspicious jump, in chapter numbers.
    #[serde(default = "default_min_gap")]
    pub min_gap: f64,
    /// Multiple of typical spacing past which a jump is suspicious.
    #[serde(default = "default_gap_factor")]
    pub gap_factor: f64,
    /// Multiple of typical spacing past which a trailing run is noise, not a continuation.
    #[serde(default = "default_sparse_factor")]
    pub sparse_factor: f64,
    /// Ceiling on the fraction of one listing a scan may reject.
    #[serde(default = "default_max_rejected_fraction")]
    pub max_rejected_fraction: f64,
}

fn default_min_sample() -> usize {
    OutlierPolicy::default().min_sample
}
fn default_min_body() -> usize {
    OutlierPolicy::default().min_body
}
fn default_min_gap() -> f64 {
    OutlierPolicy::default().min_gap
}
fn default_gap_factor() -> f64 {
    OutlierPolicy::default().gap_factor
}
fn default_sparse_factor() -> f64 {
    OutlierPolicy::default().sparse_factor
}
fn default_max_rejected_fraction() -> f64 {
    OutlierPolicy::default().max_rejected_fraction
}

impl Default for ChapterOutlierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_sample: default_min_sample(),
            min_body: default_min_body(),
            min_gap: default_min_gap(),
            gap_factor: default_gap_factor(),
            sparse_factor: default_sparse_factor(),
            max_rejected_fraction: default_max_rejected_fraction(),
        }
    }
}

impl ChapterOutlierConfig {
    /// Check that every threshold is a number the rule can actually be evaluated against.
    ///
    /// TOML spells `nan` and `inf` as ordinary float literals, and a non-finite factor does not
    /// make the rule stricter or looser — it makes every comparison in
    /// [`tankovault_domain::chapter_outliers`] false, so the guard silently stops rejecting
    /// anything while still reporting itself as enabled. A `max_rejected_fraction` above 1 is
    /// the opposite failure: it lifts the ceiling that stops a misread listing being deleted
    /// wholesale. Both are caught at boot, where the operator is still watching, rather than as
    /// a shape in the catalogue three weeks later.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] naming the first key that is out of range.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (key, value) in [
            ("min_gap", self.min_gap),
            ("gap_factor", self.gap_factor),
            ("sparse_factor", self.sparse_factor),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(ConfigError::Invalid(format!(
                    "chapter_outliers.{key} must be a finite number greater than zero, got \
                     {value}"
                )));
            }
        }
        if !(0.0..=1.0).contains(&self.max_rejected_fraction) {
            return Err(ConfigError::Invalid(format!(
                "chapter_outliers.max_rejected_fraction must be between 0 and 1, got {}",
                self.max_rejected_fraction
            )));
        }
        if self.min_body > self.min_sample {
            return Err(ConfigError::Invalid(format!(
                "chapter_outliers.min_body ({}) must not exceed min_sample ({}) — a listing \
                 large enough to judge would have less than the protected body",
                self.min_body, self.min_sample
            )));
        }
        Ok(())
    }

    /// The configured policy, as the scan engine applies it.
    ///
    /// `enabled: false` is expressed as a policy that can never fire rather than as a branch at
    /// the call site, so there is one code path through ingest whatever the configuration says.
    #[must_use]
    pub fn policy(&self) -> OutlierPolicy {
        if !self.enabled {
            return OutlierPolicy {
                min_sample: usize::MAX,
                ..OutlierPolicy::default()
            };
        }
        OutlierPolicy {
            min_sample: self.min_sample,
            min_body: self.min_body,
            min_gap: self.min_gap,
            gap_factor: self.gap_factor,
            sparse_factor: self.sparse_factor,
            max_rejected_fraction: self.max_rejected_fraction,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::chapter_outliers::implausible_indices;

    fn listing_with_stray() -> Vec<f64> {
        let mut listing: Vec<f64> = (1..=40).map(f64::from).collect();
        listing.push(9000.0);
        listing
    }

    #[test]
    fn the_defaults_match_the_domain_policy() {
        let configured = ChapterOutlierConfig::default().policy();
        let domain = OutlierPolicy::default();
        assert_eq!(configured.min_sample, domain.min_sample);
        assert_eq!(configured.min_body, domain.min_body);
        assert!((configured.sparse_factor - domain.sparse_factor).abs() < f64::EPSILON);
        assert!(
            (configured.max_rejected_fraction - domain.max_rejected_fraction).abs() < f64::EPSILON
        );
    }

    /// The kill switch has to reach the decision, not just the struct. Wired wrong, an operator
    /// who disabled the rule after a bad rejection would keep losing chapters silently.
    #[test]
    fn disabling_it_rejects_nothing() {
        let off = ChapterOutlierConfig {
            enabled: false,
            ..ChapterOutlierConfig::default()
        };
        assert!(implausible_indices(&listing_with_stray(), &off.policy()).is_empty());
    }

    /// The bug: TOML accepts `nan` as a float literal, and every comparison against `NaN` is
    /// false — so a mistyped `sparse_factor` left the guard reporting itself as enabled while
    /// rejecting nothing at all. Caught at boot instead.
    #[test]
    fn a_non_finite_threshold_is_refused() {
        assert!(ChapterOutlierConfig::default().validate().is_ok());
        for bad in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let cfg = ChapterOutlierConfig {
                sparse_factor: bad,
                ..ChapterOutlierConfig::default()
            };
            assert!(cfg.validate().is_err(), "accepted sparse_factor = {bad}");
        }
    }

    #[test]
    fn the_rejection_ceiling_stays_a_fraction() {
        for bad in [-0.1, 1.1, f64::NAN] {
            let cfg = ChapterOutlierConfig {
                max_rejected_fraction: bad,
                ..ChapterOutlierConfig::default()
            };
            assert!(
                cfg.validate().is_err(),
                "accepted max_rejected_fraction = {bad}"
            );
        }
    }

    /// A configured threshold actually moves the outcome — the knobs are not decorative.
    #[test]
    fn raising_the_sparse_factor_keeps_the_stray() {
        let lenient = ChapterOutlierConfig {
            sparse_factor: 100_000.0,
            ..ChapterOutlierConfig::default()
        };
        assert_eq!(
            implausible_indices(
                &listing_with_stray(),
                &ChapterOutlierConfig::default().policy()
            ),
            [40],
            "sanity: the defaults reject it"
        );
        assert!(implausible_indices(&listing_with_stray(), &lenient.policy()).is_empty());
    }
}
