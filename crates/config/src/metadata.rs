//! The `metadata` section: who owns each field, and which scraped terms are not the thing they
//! were scraped as.
//!
//! The section has one view per service that reads part of it. A configuration contract is a
//! claim about what one image loads, so a service embedding more of the section than its binary
//! reads would publish keys that decide nothing in its process. Every key is declared once and
//! composed into the views that need it.

use serde::Deserialize;
use tankovault_domain::{MetadataPriority, TermBlocklist};
use terrace_config::schema::Describe;

/// The whole section, as the worker's ingest path reads it: it is the one binary that decides
/// field ownership, refuses vocabulary and classifies what survives.
///
/// External sync writes the same columns, so its root takes the same `priority` subtree and
/// refusal list beside its enrichment tunables. It does not take this type whole, because it
/// classifies nothing: `is_adult` comes from `AniList`.
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
    // `nested` needs `Describe` on a `tankovault-domain` type. That crate also compiles for
    // `wasm32`, so the derive is behind its `schema` feature, which this dependency switches on.
    // The browser bundle reaches the same types through `crates/api-client` with the feature off.
    #[config(nested)]
    #[serde(default)]
    pub priority: MetadataPriority,
    /// Vocabulary guard: which scraped terms intake refuses, as tags and as credits alike.
    #[config(nested)]
    #[serde(default)]
    pub tags: TagIntakeConfig,
}

impl MetadataPriorityConfig {
    /// Returns the tag guard this configuration resolves to, built on every call.
    ///
    /// Intake reads it once per scan.
    #[must_use]
    pub fn term_blocklist(&self) -> TermBlocklist {
        self.tags.blocklist()
    }
}

/// The section as a service that only *reads* metadata needs it: the adult classifier alone.
///
/// The API is the only reader. It writes no metadata, so `metadata.priority` decides nothing in
/// its process, and it reads the vocabulary guard only to learn which genres put a series behind
/// the gate.
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
/// The half of `metadata.tags` the read side needs. [`TagIntakeConfig`] flattens it back in, so
/// `metadata.tags.adult_tags` has one declaration.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct AdultTagConfig {
    /// Additional genre chips that classify a series as adult, matched on their slug.
    ///
    /// These add to the shipped terms in [`tankovault_domain::AdultTagSet`], and no switch drops
    /// them. An emptied classifier would stop classifying silently, while the two supported ways
    /// to show adult content, the deployment flag and the per-reader opt-in, both record a
    /// decision.
    #[serde(default)]
    pub adult_tags: Vec<String>,
}

impl AdultTagConfig {
    /// Resolves the adult classifier intake and the read side apply.
    #[must_use]
    pub fn adult_tags(&self) -> tankovault_domain::AdultTagSet {
        tankovault_domain::AdultTagSet::with_extra(self.adult_tags.iter())
    }
}

/// The vocabulary guard, as the services that *write* metadata apply it.
///
/// The shipped defaults are the terms no catalogue wants, such as a scrape template's own field
/// labels. The extra list is what the operator's providers emit. The extra list adds to the
/// defaults, so an operator adding one term keeps the rest.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct TermBlocklistConfig {
    /// Whether the shipped [`tankovault_domain::DEFAULT_BLOCKED_TERMS`] apply.
    ///
    /// Turn it off for a catalogue that uses one of them as a genre. Turning it off stops the
    /// refusal from the next scan on and leaves stored tags and credits as they are.
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
    /// Resolves the two lists into the guard intake applies.
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
/// The worker refuses vocabulary at intake and classifies what survives, so it reads every key
/// here. The halves are flattened, so the paths stay `metadata.tags.blocklist` and
/// `metadata.tags.adult_tags` for every service.
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
    /// Resolves the two lists into the guard intake applies.
    #[must_use]
    pub fn blocklist(&self) -> TermBlocklist {
        self.terms.blocklist()
    }

    /// Resolves the adult classifier intake applies.
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

    /// An operator's own terms add to the shipped ones. Treating the configured list as the whole
    /// blocklist would turn "also refuse `Bookmark`" into "refuse only `Bookmark`".
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

    /// An absent `[metadata.tags]`, the common case, still produces the guard.
    #[test]
    fn an_absent_section_still_guards() {
        assert!(
            MetadataPriorityConfig::default()
                .term_blocklist()
                .blocks("Status")
        );
    }

    /// Both flattened halves load from the environment.
    ///
    /// A flattened field deserialises through serde's buffered content. If that dropped a value,
    /// the schema would still publish `metadata.tags.blocklist` and the worker would boot with an
    /// empty guard.
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

    /// The API and the worker read `metadata.tags.adult_tags` through different section types
    /// and resolve the same key. Two keys would open the gate for the genres the reader's tag
    /// facet still hides.
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

    /// A default [`AdultTagConfig`] classifies with the shipped terms.
    #[test]
    fn an_absent_classifier_section_still_classifies() {
        assert!(AdultTagConfig::default().adult_tags().classifies("hentai"));
    }
}
