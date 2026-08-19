//! The `metadata` section as far as every writer of series metadata cares: who owns each field,
//! and which scraped terms are not the thing they were scraped as.

use serde::Deserialize;
use tankovault_domain::{MetadataPriority, TermBlocklist};
use terrace_config::schema::Describe;

/// Per-field source authority and the intake vocabulary guard, read by **both** the worker's
/// ingest path and external sync's enrichment writer.
///
/// Shared deliberately, for the same reason as [`crate::MatchingConfig`]: the two paths write
/// the same columns, and a rule only one of them consults is not a rule. The sync service
/// composes this section with its own enrichment-sweep tunables.
///
/// ```
/// use tankovault_config::MetadataPriorityConfig;
///
/// // Shipped defaults refuse the placeholder a scrape template leaves behind.
/// let config = MetadataPriorityConfig::default();
/// assert!(config.term_blocklist().blocks("Updating"));
/// assert!(!config.term_blocklist().blocks("Romance"));
/// ```
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct MetadataPriorityConfig {
    /// Per-field source authority order (default: `AniList` before the adapters).
    // Deliberately a leaf rather than `#[config(nested)]`: the type is `tankovault-domain`'s,
    // and describing it would put `terrace-config` — and figment with it — into the workspace's
    // leaf crate. The contract therefore publishes `metadata.priority` with no constraint, which
    // is the honest answer: a consumer can see the key exists and cannot check its shape.
    #[serde(default)]
    pub priority: MetadataPriority,
    /// Vocabulary guard: which scraped terms intake refuses, as tags and as credits alike.
    #[config(nested)]
    #[serde(default)]
    pub tags: TagIntakeConfig,
}

impl MetadataPriorityConfig {
    /// The tag guard this configuration resolves to.
    ///
    /// Built per call rather than cached: intake reads it once per scan, not per tag, and a
    /// stored copy is one more thing a config reload has to remember to replace.
    #[must_use]
    pub fn term_blocklist(&self) -> TermBlocklist {
        self.tags.blocklist()
    }
}

/// Operator control over the vocabulary intake is allowed to create.
///
/// Two lists rather than one overridable list, because the shipped defaults and an operator's
/// additions answer different questions: the defaults are the terms *no* catalogue wants (a
/// scrape template's own field labels), and the extra list is whatever the operator's own
/// providers turn out to emit. Making one replace the other would mean an operator adding a
/// single term silently loses the rest.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct TagIntakeConfig {
    /// Whether the shipped [`tankovault_domain::DEFAULT_BLOCKED_TERMS`] apply.
    ///
    /// An escape hatch for a deployment whose catalogue genuinely uses one of them as a genre.
    /// Turning it off does not remove what is already stored — nothing in the normal path
    /// retracts a tag or a credit — but stops it being refused from the next scan onwards.
    #[serde(default = "crate::default_true")]
    pub use_defaults: bool,
    /// Additional refused terms, matched on their slug: `N/A`, `n/a` and `n-a` are one entry.
    #[serde(default)]
    pub blocklist: Vec<String>,
    /// Additional genre chips that classify a series as adult, matched on their slug.
    ///
    /// Additions only: unlike [`Self::blocklist`] there is no switch that drops the shipped
    /// terms. See [`tankovault_domain::AdultTagSet`] — an emptied classifier silently stops
    /// classifying, where the two supported ways to make adult content visible (the deployment
    /// flag and the per-reader opt-in) both leave a record of somebody deciding.
    #[serde(default)]
    pub adult_tags: Vec<String>,
}

impl Default for TagIntakeConfig {
    fn default() -> Self {
        Self {
            use_defaults: true,
            blocklist: Vec::new(),
            adult_tags: Vec::new(),
        }
    }
}

impl TagIntakeConfig {
    /// Resolve the two lists into the guard intake applies.
    #[must_use]
    pub fn blocklist(&self) -> TermBlocklist {
        let defaults = if self.use_defaults {
            tankovault_domain::DEFAULT_BLOCKED_TERMS
        } else {
            &[]
        };
        TermBlocklist::new(
            defaults
                .iter()
                .map(|term| (*term).to_owned())
                .chain(self.blocklist.iter().cloned()),
        )
    }

    /// Resolve the adult classifier intake applies.
    #[must_use]
    pub fn adult_tags(&self) -> tankovault_domain::AdultTagSet {
        tankovault_domain::AdultTagSet::with_extra(self.adult_tags.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::{MetadataPriorityConfig, TagIntakeConfig};

    /// An operator's own terms must *add* to the shipped ones. The natural implementation —
    /// treating the configured list as the whole blocklist — turns "also refuse `Bookmark`"
    /// into "refuse only `Bookmark`", and the defaults stop applying with nothing said.
    #[test]
    fn operator_terms_extend_the_defaults_rather_than_replacing_them() {
        let config = TagIntakeConfig {
            use_defaults: true,
            blocklist: vec!["Bookmark".to_owned()],
            adult_tags: Vec::new(),
        };
        let list = config.blocklist();
        assert!(list.blocks("Bookmark"));
        assert!(list.blocks("Updating"));
    }

    /// Switching the defaults off must leave only what the operator wrote, including nothing.
    #[test]
    fn disabling_the_defaults_leaves_only_the_configured_terms() {
        let only_mine = TagIntakeConfig {
            use_defaults: false,
            blocklist: vec!["Bookmark".to_owned()],
            adult_tags: Vec::new(),
        };
        let list = only_mine.blocklist();
        assert!(list.blocks("Bookmark"));
        assert!(!list.blocks("Updating"));

        let off = TagIntakeConfig {
            use_defaults: false,
            blocklist: Vec::new(),
            adult_tags: Vec::new(),
        };
        assert!(off.blocklist().is_empty());
    }

    /// The section is `#[serde(default)]` everywhere, so an absent `[metadata.tags]` must
    /// still produce the guard — a deployment that never wrote the section is the common case.
    #[test]
    fn an_absent_section_still_guards() {
        assert!(
            MetadataPriorityConfig::default()
                .term_blocklist()
                .blocks("Status")
        );
    }
}
