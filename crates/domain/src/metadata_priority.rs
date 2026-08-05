//! Which upstream source has the final say on each piece of series metadata — domain policy,
//! not deployment wiring. Sources are a closed enum, so a typo in a priority list fails at
//! load instead of silently degrading to "no listed source matched".
//!
//! [`MetadataPriority::resolve`] answers with the winning *source* as well as the value: a
//! writer that does not store which source won cannot honour this policy on the next write,
//! and the field silently reverts to last-writer-wins.

use crate::{ContentType, SeriesStatus};
use serde::Deserialize;

/// An upstream that can supply series metadata.
///
/// Closed by design: adding a source is a code change anyway, so there's no value in accepting
/// arbitrary strings — only the risk of a typo read as deliberate de-prioritisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
// `sqlx` on maps this to the `metadata_source` Postgres enum the provenance columns are
// declared as; off (the WASM frontend) keeps this crate free of the native sqlx stack.
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "metadata_source", rename_all = "lowercase")
)]
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
/// Every field a second source can also write needs a variant: a field without one cannot be
/// resolved at all, which is how it ends up back on last-writer-wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataField {
    /// The long-form series description.
    Description,
    /// The canonical/display title.
    Title,
    /// The cover image URL.
    Cover,
    /// The medium/origin classification.
    ContentType,
    /// The work's publication status.
    Status,
    /// The year of first publication.
    ReleaseYear,
}

/// Whether a candidate value is an actual answer rather than a placeholder.
///
/// Defined per type so "upstream had no opinion" has one meaning: a blank string and an
/// `Unknown` enum are both absences, and letting either count as an answer is exactly how a
/// placeholder outranks real data.
pub trait MetadataValue {
    /// Whether this value carries information a source is entitled to win with.
    fn is_answer(&self) -> bool;
}

impl MetadataValue for str {
    fn is_answer(&self) -> bool {
        !self.trim().is_empty()
    }
}

impl MetadataValue for String {
    fn is_answer(&self) -> bool {
        self.as_str().is_answer()
    }
}

impl<T: MetadataValue + ?Sized> MetadataValue for &T {
    fn is_answer(&self) -> bool {
        (**self).is_answer()
    }
}

impl MetadataValue for ContentType {
    fn is_answer(&self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

impl MetadataValue for SeriesStatus {
    fn is_answer(&self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

impl MetadataValue for i32 {
    fn is_answer(&self) -> bool {
        true
    }
}

/// Per-field source authority: an ordered preference list, highest priority first. The first
/// source that supplies a real value wins; ships with `AniList` before the adapters.
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
    /// Priority order for the medium/origin classification.
    #[serde(default = "MetadataPriority::default_order")]
    pub content_type: Vec<MetadataSource>,
    /// Priority order for the publication status.
    #[serde(default = "MetadataPriority::default_order")]
    pub status: Vec<MetadataSource>,
    /// Priority order for the year of first publication.
    #[serde(default = "MetadataPriority::default_order")]
    pub release_year: Vec<MetadataSource>,
    /// Fallback order for any field whose list was explicitly emptied.
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
            MetadataField::ContentType => &self.content_type,
            MetadataField::Status => &self.status,
            MetadataField::ReleaseYear => &self.release_year,
        };
        if list.is_empty() { &self.default } else { list }
    }

    /// Pick the winning source and value for `field`: the first source in its priority order
    /// supplying a real value wins.
    ///
    /// Candidates are scanned in the caller's order within one source, so a writer listing its
    /// own incoming value before the stored one refreshes a field it already owns.
    ///
    /// If no prioritised source matches, any present candidate is used as a last resort, so
    /// a partial order (say `[adapter]`) de-prioritises other sources rather than discarding
    /// them.
    #[must_use]
    pub fn resolve<T: MetadataValue + Clone>(
        &self,
        field: MetadataField,
        candidates: &[(MetadataSource, Option<T>)],
    ) -> Option<(MetadataSource, T)> {
        for source in self.order_for(field) {
            for (candidate_source, value) in candidates {
                if candidate_source == source
                    && let Some(v) = value
                    && v.is_answer()
                {
                    return Some((*candidate_source, v.clone()));
                }
            }
        }
        candidates.iter().find_map(|(source, value)| {
            value
                .as_ref()
                .filter(|v| MetadataValue::is_answer(*v))
                .map(|v| (*source, v.clone()))
        })
    }
}

impl Default for MetadataPriority {
    fn default() -> Self {
        Self {
            description: Self::default_order(),
            title: Self::default_order(),
            cover: Self::default_order(),
            content_type: Self::default_order(),
            status: Self::default_order(),
            release_year: Self::default_order(),
            default: Self::default_order(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MetadataField, MetadataPriority, MetadataSource};
    use crate::ContentType;

    fn resolved(
        cfg: &MetadataPriority,
        field: MetadataField,
        candidates: &[(MetadataSource, Option<&str>)],
    ) -> Option<String> {
        cfg.resolve(field, candidates).map(|(_, v)| v.to_owned())
    }

    #[test]
    fn the_default_order_prefers_anilist_over_the_adapters() {
        let cfg = MetadataPriority::default();
        let winner = cfg.resolve(
            MetadataField::Description,
            &[
                (MetadataSource::Adapter, Some("scraped blurb")),
                (MetadataSource::AniList, Some("anilist blurb")),
            ],
        );
        assert_eq!(winner, Some((MetadataSource::AniList, "anilist blurb")));
    }

    #[test]
    fn a_blank_or_absent_higher_priority_value_falls_through() {
        let cfg = MetadataPriority::default();
        let winner = resolved(
            &cfg,
            MetadataField::Description,
            &[
                (MetadataSource::AniList, Some("   ")),
                (MetadataSource::Adapter, Some("scraped blurb")),
            ],
        );
        assert_eq!(winner.as_deref(), Some("scraped blurb"));

        let winner_none = resolved(
            &cfg,
            MetadataField::Description,
            &[
                (MetadataSource::AniList, None),
                (MetadataSource::Adapter, Some("only one")),
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
        let winner = resolved(
            &cfg,
            MetadataField::Description,
            &[
                (MetadataSource::AniList, Some("anilist blurb")),
                (MetadataSource::Adapter, Some("scraped blurb")),
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
        let winner = resolved(
            &cfg,
            MetadataField::Description,
            &[(MetadataSource::AniList, Some("anilist blurb"))],
        );
        assert_eq!(winner.as_deref(), Some("anilist blurb"));
        assert_eq!(
            resolved(
                &cfg,
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

    /// The winner's source is what the caller stores as provenance, so an unlisted last-resort
    /// win must report the source that actually supplied the value, not the head of the order.
    #[test]
    fn the_winning_source_is_reported_with_the_value() {
        let cfg = MetadataPriority {
            description: vec![MetadataSource::Adapter],
            ..MetadataPriority::default()
        };
        assert_eq!(
            cfg.resolve(
                MetadataField::Description,
                &[(MetadataSource::AniList, Some("anilist blurb"))]
            ),
            Some((MetadataSource::AniList, "anilist blurb"))
        );
    }

    /// `Unknown` is the adapters' hardcoded "no selector for this", not a classification. Read
    /// as an answer it outranks `AniList`'s real one, which is how every enriched `content_type`
    /// used to revert to `unknown` on the next scan.
    #[test]
    fn an_unknown_enum_variant_is_an_absence_not_an_answer() {
        let cfg = MetadataPriority::default();
        let winner = cfg.resolve(
            MetadataField::ContentType,
            &[
                (MetadataSource::Adapter, Some(ContentType::Unknown)),
                (MetadataSource::AniList, Some(ContentType::Manhwa)),
            ],
        );
        assert_eq!(winner, Some((MetadataSource::AniList, ContentType::Manhwa)));

        // And with nothing but placeholders, nobody wins — writing `unknown` back over a real
        // stored value is the same bug from the other side.
        assert_eq!(
            cfg.resolve(
                MetadataField::ContentType,
                &[(MetadataSource::Adapter, Some(ContentType::Unknown))]
            ),
            None
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
