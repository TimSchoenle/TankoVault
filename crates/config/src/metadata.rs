//! The `metadata` section: who owns each field, and which scraped terms are not the thing they
//! were scraped as.
//!
//! Four types over one key namespace, one per service that reads part of it. A configuration
//! contract is a claim about what *one image* loads and a chart wires values from it, so a
//! service that embeds more of this section than its binary consults publishes keys that decide
//! nothing in its process. Every key below is declared exactly once and composed into the views
//! that need it, so the three services cannot come to describe it three ways.

use serde::Deserialize;
use tankovault_domain::{MetadataPriority, TermBlocklist};
use terrace_config::schema::Describe;

/// The whole section, as the worker's ingest path reads it: it is the one binary that decides
/// field ownership, refuses vocabulary and classifies what survives.
///
/// External sync writes the same columns and so takes the same `priority` subtree and the same
/// refusal list — restated in its own root beside its enrichment tunables rather than shared
/// whole, because it classifies nothing: `is_adult` comes from `AniList`.
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
    // `nested`, which needs `Describe` on a `tankovault-domain` type. That crate also compiles
    // for `wasm32`, so the derive is behind its `schema` feature and this dependency is what
    // switches it on; the browser bundle reaches the same types through `crates/api-client` with
    // the feature off and links no figment. The seven keys under here are one per metadata field,
    // each a list of the two `MetadataSource` spellings.
    #[config(nested)]
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

/// The section as a service that only *reads* metadata needs it: the adult classifier alone.
///
/// The API is the whole audience. It writes no metadata, so `metadata.priority` decides nothing
/// in its process; it consults the vocabulary guard only to learn which genres put a series
/// behind the gate. Embedding [`MetadataPriorityConfig`] here — how it held the section before —
/// had `api`'s contract claim its image reads seven keys the binary has never opened.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct MetadataReadConfig {
    /// The adult classifier, shared with the worker so the genres the public tag facet withholds
    /// are exactly the ones that put a series behind the gate.
    #[config(nested)]
    #[serde(default)]
    pub tags: AdultTagConfig,
}

/// The genre chips that classify a series as adult.
///
/// Its own type because it is the half of `metadata.tags` the read side needs;
/// [`TagIntakeConfig`] flattens it back in, so `metadata.tags.adult_tags` has one declaration
/// and the API's contract and the worker's cannot describe it two ways.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct AdultTagConfig {
    /// Additional genre chips that classify a series as adult, matched on their slug.
    ///
    /// Additions only: unlike the refusal list in [`TermBlocklistConfig`] there is no switch
    /// that drops the shipped terms. See [`tankovault_domain::AdultTagSet`] — an emptied classifier silently
    /// stops classifying, where the two supported ways to make adult content visible (the
    /// deployment flag and the per-reader opt-in) both leave a record of somebody deciding.
    #[serde(default)]
    pub adult_tags: Vec<String>,
}

impl AdultTagConfig {
    /// Resolve the adult classifier intake and the read side apply.
    #[must_use]
    pub fn adult_tags(&self) -> tankovault_domain::AdultTagSet {
        tankovault_domain::AdultTagSet::with_extra(self.adult_tags.iter())
    }
}

/// The vocabulary guard, as the services that *write* metadata apply it.
///
/// Two lists rather than one overridable list, because the shipped defaults and an operator's
/// additions answer different questions: the defaults are the terms *no* catalogue wants (a
/// scrape template's own field labels), and the extra list is whatever the operator's own
/// providers turn out to emit. Making one replace the other would mean an operator adding a
/// single term silently loses the rest.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct TermBlocklistConfig {
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
}

impl Default for TermBlocklistConfig {
    fn default() -> Self {
        Self {
            use_defaults: true,
            blocklist: Vec::new(),
        }
    }
}

impl TermBlocklistConfig {
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
}

/// The whole `metadata.tags` subtree, for the one service that applies all of it.
///
/// The worker both refuses vocabulary at intake and classifies what survives, so it is the only
/// binary that reads every key here. The halves are flattened rather than nested so the paths
/// stay `metadata.tags.blocklist` and `metadata.tags.adult_tags` for every service, and each key
/// keeps exactly one declaration.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct TagIntakeConfig {
    /// What intake refuses.
    #[config(nested)]
    #[serde(flatten)]
    pub terms: TermBlocklistConfig,
    /// What intake classifies as adult.
    #[config(nested)]
    #[serde(flatten)]
    pub adult: AdultTagConfig,
}

impl TagIntakeConfig {
    /// Resolve the two lists into the guard intake applies.
    #[must_use]
    pub fn blocklist(&self) -> TermBlocklist {
        self.terms.blocklist()
    }

    /// Resolve the adult classifier intake applies.
    #[must_use]
    pub fn adult_tags(&self) -> tankovault_domain::AdultTagSet {
        self.adult.adult_tags()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdultTagConfig, MetadataPriorityConfig, MetadataReadConfig, TagIntakeConfig,
        TermBlocklistConfig,
    };
    use serde::Deserialize;
    use terrace_config::testing::Harness;

    /// An operator's own terms must *add* to the shipped ones. The natural implementation —
    /// treating the configured list as the whole blocklist — turns "also refuse `Bookmark`"
    /// into "refuse only `Bookmark`", and the defaults stop applying with nothing said.
    #[test]
    fn operator_terms_extend_the_defaults_rather_than_replacing_them() {
        let config = TermBlocklistConfig {
            use_defaults: true,
            blocklist: vec!["Bookmark".to_owned()],
        };
        let list = config.blocklist();
        assert!(list.blocks("Bookmark"));
        assert!(list.blocks("Updating"));
    }

    /// Switching the defaults off must leave only what the operator wrote, including nothing.
    #[test]
    fn disabling_the_defaults_leaves_only_the_configured_terms() {
        let only_mine = TermBlocklistConfig {
            use_defaults: false,
            blocklist: vec!["Bookmark".to_owned()],
        };
        let list = only_mine.blocklist();
        assert!(list.blocks("Bookmark"));
        assert!(!list.blocks("Updating"));

        let off = TermBlocklistConfig {
            use_defaults: false,
            blocklist: Vec::new(),
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
    /// The two halves of `metadata.tags` are `#[serde(flatten)]`, and a flattened field is
    /// deserialised through serde's buffered content rather than straight off the figment value.
    /// That is the one thing about this split that could break silently: the schema would still
    /// publish `metadata.tags.blocklist`, the contract would still name it, and the worker would
    /// boot with an empty guard because the value never reached the field.
    #[test]
    fn both_flattened_halves_load_from_the_environment() {
        #[derive(Debug, Deserialize)]
        struct Sample {
            #[serde(default)]
            tags: TagIntakeConfig,
        }

        Harness::over(crate::terrace()).run(|jail| {
            jail.env_key("tags.use_defaults", false);
            jail.env_key("tags.blocklist", r#"["Bookmark"]"#);
            jail.env_key("tags.adult_tags", r#"["Cooking"]"#);

            let cfg: Sample = crate::load()?;
            assert!(!cfg.tags.terms.use_defaults);
            assert_eq!(cfg.tags.terms.blocklist, ["Bookmark"]);
            assert_eq!(cfg.tags.adult.adult_tags, ["Cooking"]);
            assert!(cfg.tags.blocklist().blocks("Bookmark"));
            assert!(cfg.tags.adult_tags().classifies("cooking"));
            Ok(())
        });
    }

    /// `metadata.tags.adult_tags` is declared once and read through three different section
    /// types, because a service's configuration contract is a claim about what its image loads
    /// and each service publishes only the keys it consults. The spelling has to survive that:
    /// a rename that reached [`TagIntakeConfig`] but not [`AdultTagConfig`] would leave the
    /// worker and the API classifying on two different keys, and the gate would open for
    /// exactly the genres the reader's tag facet still hides.
    #[test]
    fn the_reader_and_the_writer_resolve_the_same_adult_tag_key() {
        #[derive(Debug, Deserialize)]
        struct Reader {
            #[serde(default)]
            metadata: MetadataReadConfig,
        }
        #[derive(Debug, Deserialize)]
        struct Writer {
            #[serde(default)]
            metadata: MetadataPriorityConfig,
        }

        Harness::over(crate::terrace()).run(|jail| {
            jail.env_key("metadata.tags.adult_tags", r#"["Cooking"]"#);

            let reader: Reader = crate::load()?;
            let writer: Writer = crate::load()?;
            assert_eq!(
                reader.metadata.tags.adult_tags,
                writer.metadata.tags.adult.adult_tags
            );
            assert!(reader.metadata.tags.adult_tags().classifies("cooking"));
            Ok(())
        });
    }

    /// A default [`AdultTagConfig`] still classifies: the shipped terms are the whole point, and
    /// this side has no `use_defaults` switch to lose them through.
    #[test]
    fn an_absent_classifier_section_still_classifies() {
        assert!(AdultTagConfig::default().adult_tags().classifies("hentai"));
    }
}
