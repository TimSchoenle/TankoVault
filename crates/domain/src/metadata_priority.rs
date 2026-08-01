//! Which upstream source has the final say on each piece of series metadata — domain policy,
//! not deployment wiring. Sources are a closed enum, so a typo in a priority list fails at
//! load instead of silently degrading to "no listed source matched".

use serde::Deserialize;

/// An upstream that can supply series metadata.
///
/// Closed by design: adding a source is a code change anyway, so there's no value in accepting
/// arbitrary strings — only the risk of a typo read as deliberate de-prioritisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetadataSource {
    /// `AniList`, via its public GraphQL metadata or a linked user's list.
    #[serde(rename = "anilist")]
    AniList,
    /// The local provider adapters that scrape a source site.
    Adapter,
}

impl MetadataSource {
    /// The stable wire/config spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AniList => "anilist",
            Self::Adapter => "adapter",
        }
    }
}

/// A piece of series metadata whose source can be prioritised independently.
///
/// Fields not listed here follow [`MetadataPriority::default_order`] and need no variant —
/// add one only when a field earns its own configurable order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataField {
    /// The long-form series description.
    Description,
    /// The canonical/display title.
    Title,
    /// The cover image URL.
    Cover,
}

/// Per-field source authority: an ordered preference list, highest priority first. The first
/// source that supplies a non-blank value wins; ships with `AniList` before the adapters.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataPriority {
    /// Priority order for the long-form series description.
    #[serde(default = "MetadataPriority::default_order")]
    pub description: Vec<MetadataSource>,
    /// Priority order for the canonical/display title.
    #[serde(default = "MetadataPriority::default_order")]
    pub title: Vec<MetadataSource>,
    /// Priority order for the cover image URL.
    #[serde(default = "MetadataPriority::default_order")]
    pub cover: Vec<MetadataSource>,
    /// Fallback order for any field without its own list, or whose list was emptied.
    #[serde(default = "MetadataPriority::default_order")]
    pub default: Vec<MetadataSource>,
}

impl MetadataPriority {
    /// The default preference: `AniList` wins, then the scraping adapters.
    #[must_use]
    pub fn default_order() -> Vec<MetadataSource> {
        vec![MetadataSource::AniList, MetadataSource::Adapter]
    }

    /// Priority order for `field`, falling back to [`Self::default`] when its own list was
    /// explicitly emptied.
    #[must_use]
    pub fn order_for(&self, field: MetadataField) -> &[MetadataSource] {
        let list = match field {
            MetadataField::Description => &self.description,
            MetadataField::Title => &self.title,
            MetadataField::Cover => &self.cover,
        };
        if list.is_empty() { &self.default } else { list }
    }

    /// Pick the winning value for `field`: the first source in its priority order supplying
    /// a non-blank value wins.
    ///
    /// If no prioritised source matches, any present candidate is used as a last resort, so
    /// a partial order (say `[adapter]`) de-prioritises other sources rather than discarding
    /// them.
    #[must_use]
    pub fn resolve(
        &self,
        field: MetadataField,
        candidates: &[(MetadataSource, Option<String>)],
    ) -> Option<String> {
        for source in self.order_for(field) {
            for (candidate_source, value) in candidates {
                if candidate_source == source
                    && let Some(v) = value
                    && !v.trim().is_empty()
                {
                    return Some(v.clone());
                }
            }
        }
        candidates
            .iter()
            .find_map(|(_, v)| v.clone().filter(|s| !s.trim().is_empty()))
    }
}

impl Default for MetadataPriority {
    fn default() -> Self {
        Self {
            description: Self::default_order(),
            title: Self::default_order(),
            cover: Self::default_order(),
            default: Self::default_order(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MetadataField, MetadataPriority, MetadataSource};

    #[test]
    fn the_default_order_prefers_anilist_over_the_adapters() {
        let cfg = MetadataPriority::default();
        let winner = cfg.resolve(
            MetadataField::Description,
            &[
                (MetadataSource::Adapter, Some("scraped blurb".to_owned())),
                (MetadataSource::AniList, Some("anilist blurb".to_owned())),
            ],
        );
        assert_eq!(winner.as_deref(), Some("anilist blurb"));
    }

    #[test]
    fn a_blank_or_absent_higher_priority_value_falls_through() {
        let cfg = MetadataPriority::default();
        let winner = cfg.resolve(
            MetadataField::Description,
            &[
                (MetadataSource::AniList, Some("   ".to_owned())),
                (MetadataSource::Adapter, Some("scraped blurb".to_owned())),
            ],
        );
        assert_eq!(winner.as_deref(), Some("scraped blurb"));

        let winner_none = cfg.resolve(
            MetadataField::Description,
            &[
                (MetadataSource::AniList, None),
                (MetadataSource::Adapter, Some("only one".to_owned())),
            ],
        );
        assert_eq!(winner_none.as_deref(), Some("only one"));
    }

    #[test]
    fn a_configured_adapter_first_order_is_respected() {
        let cfg = MetadataPriority {
            description: vec![MetadataSource::Adapter, MetadataSource::AniList],
            ..MetadataPriority::default()
        };
        let winner = cfg.resolve(
            MetadataField::Description,
            &[
                (MetadataSource::AniList, Some("anilist blurb".to_owned())),
                (MetadataSource::Adapter, Some("scraped blurb".to_owned())),
            ],
        );
        assert_eq!(winner.as_deref(), Some("scraped blurb"));
    }

    /// A source left out of a field's order is de-prioritised, not discarded — otherwise
    /// narrowing one field's list would silently drop metadata nothing else supplies.
    #[test]
    fn an_unlisted_source_is_still_used_as_a_last_resort() {
        let cfg = MetadataPriority {
            description: vec![MetadataSource::Adapter],
            ..MetadataPriority::default()
        };
        let winner = cfg.resolve(
            MetadataField::Description,
            &[(MetadataSource::AniList, Some("anilist blurb".to_owned()))],
        );
        assert_eq!(winner.as_deref(), Some("anilist blurb"));
        assert_eq!(
            cfg.resolve(
                MetadataField::Description,
                &[(MetadataSource::AniList, None)]
            ),
            None
        );
    }

    #[test]
    fn an_emptied_list_falls_back_to_the_default_order() {
        let cfg = MetadataPriority {
            description: Vec::new(),
            ..MetadataPriority::default()
        };
        assert_eq!(
            cfg.order_for(MetadataField::Description),
            cfg.default.as_slice()
        );
    }

    /// A misspelled source is a load-time error, not a silent de-prioritisation of the
    /// source the operator meant to name.
    #[test]
    fn an_unknown_source_is_rejected_at_load_rather_than_ignored() {
        let ok: Result<MetadataPriority, _> =
            serde_json::from_str(r#"{"description":["adapter","anilist"]}"#);
        assert!(ok.is_ok());

        let typo: Result<MetadataPriority, _> =
            serde_json::from_str(r#"{"description":["anilst"]}"#);
        assert!(typo.is_err(), "a typo must not parse");
    }
}
