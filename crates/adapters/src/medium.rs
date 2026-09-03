//! Telling a site's prose apart from its comics.
//!
//! Scanlator sites routinely sell text novels from the same catalogue, under the same URL
//! prefix, as the comics they are known for. There is nothing here to track — a novel has no
//! pages — so the rows have to be dropped before they are registered.
//!
//! Two shapes of signal, one vocabulary. A platform that publishes a medium field is read by
//! its own adapter through [`is_prose_medium`]; a site that publishes none marks prose in the
//! listed title instead, which is the only signal a selector-driven adapter has.
//! [`ProseFiltered`] applies that one to every adapter at [`crate::build_adapter`], so a family
//! preset does not have to opt in.

use crate::error::AdapterError;
use crate::types::{CatalogPage, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter};
use async_trait::async_trait;

/// Medium labels that name prose, in the normalised spelling [`normalise`] produces.
const PROSE_MEDIA: &[&str] = &[
    "novel",
    "novels",
    "light novel",
    "web novel",
    "webnovel",
    "ln",
];

/// Lower-case, with `_`/`-`/whitespace folded to single spaces, so a platform's `LIGHT_NOVEL`
/// and a title's `[Light Novel]` are the same token.
fn normalise(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for ch in label.chars() {
        if ch == '_' || ch == '-' || ch.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// Whether a medium label the provider itself publishes names prose.
pub(crate) fn is_prose_medium(label: &str) -> bool {
    PROSE_MEDIA.contains(&normalise(label).as_str())
}

/// Whether a listed title carries the site's own prose marker — `The Former Supreme Master
/// [Novel]`, as `thunderscans` lists all seven of its novels.
///
/// Only a bracketed tag counts, and only when the whole tag is a medium. The word alone is not
/// a marker: *The Novel's Extra* and *Novel Instructor* are comics, and a substring test drops
/// them. `[Novel Adaptation]` is likewise kept — it describes the source, not the medium.
pub(crate) fn is_prose_title(title: &str) -> bool {
    let mut rest = title;
    while let Some(open) = rest.find(['[', '(']) {
        let closing = if rest[open..].starts_with('[') {
            ']'
        } else {
            ')'
        };
        let after = &rest[open + 1..];
        // An unmatched bracket is markup noise: step past it and keep looking, rather than
        // letting it hide a marker further along the title.
        let Some(close) = after.find(closing) else {
            rest = after;
            continue;
        };
        if is_prose_medium(&after[..close]) {
            return true;
        }
        rest = &after[close + 1..];
    }
    false
}

/// A [`SourceAdapter`] with the prose rows dropped from both of its listings.
///
/// Applied to every adapter rather than to the config-driven one alone: the marker is a
/// convention of the sites, not of a theme, and a guard only some adapters carry is one the
/// next provider silently does without.
pub(crate) struct ProseFiltered(Box<dyn SourceAdapter>);

impl ProseFiltered {
    /// Wrap `inner`, which is where every built adapter passes exactly once.
    pub(crate) fn wrap(inner: Box<dyn SourceAdapter>) -> Box<dyn SourceAdapter> {
        Box::new(Self(inner))
    }
}

/// Drop the prose rows from a listing, logging how many went and for which provider.
fn retain_comics<T>(ctx: &Ctx, rows: &mut Vec<T>, title: impl Fn(&T) -> &str) {
    let before = rows.len();
    rows.retain(|row| !is_prose_title(title(row)));
    let dropped = before - rows.len();
    if dropped > 0 {
        tracing::debug!(
            provider = %ctx.provider_slug,
            dropped,
            of = before,
            "skipping listing rows the site marks as prose"
        );
    }
}

#[async_trait]
impl SourceAdapter for ProseFiltered {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let mut catalog = self.0.list_catalog(ctx, page).await?;
        // After the inner adapter has decided `has_next`, never before it: an adapter with no
        // next-page marker reads "this page yielded nothing" as the end of the catalogue, so a
        // page of nothing but novels would end the walk with the rest of the site unseen.
        retain_comics(ctx, &mut catalog.items, |item| item.title.as_str());
        Ok(catalog)
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let mut updates = self.0.list_latest(ctx).await?;
        retain_comics(ctx, &mut updates, |update| update.title.as_str());
        Ok(updates)
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        self.0.fetch_series(ctx, path).await
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        self.0.fetch_chapters(ctx, path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platforms_own_medium_field_is_read_in_every_spelling_it_uses() {
        assert!(is_prose_medium("NOVEL"));
        assert!(is_prose_medium("light_novel"));
        assert!(is_prose_medium("Web Novel"));
        assert!(is_prose_medium(" novel "));
        assert!(!is_prose_medium("MANHWA"));
        assert!(!is_prose_medium(""));
    }

    /// Regression: `thunderscans` sells prose from the `/comics/` catalogue that carries its
    /// comics, marking it in the title alone — `The Former Supreme Master [Novel]`, whose
    /// chapters are text pages this application cannot track. All seven of its novels were
    /// ingested as series.
    #[test]
    fn a_bracketed_medium_marks_the_title_as_prose() {
        assert!(is_prose_title("The Former Supreme Master [Novel]"));
        assert!(is_prose_title("The Academy's Unrivaled Professor [Novel]"));
        assert!(is_prose_title("Some Title (Light Novel)"));
        assert!(is_prose_title("Some Title [LN]"));
    }

    /// The discriminating case, and the reason the tag must be bracketed *and* whole: three of
    /// these are comics whose titles a substring test would drop.
    #[test]
    fn the_word_novel_in_a_title_is_not_a_marker() {
        assert!(!is_prose_title("The Novel's Extra"));
        assert!(!is_prose_title("Novel Instructor"));
        assert!(!is_prose_title("Kill the Villainess [Novel Adaptation]"));
        assert!(!is_prose_title("Solo Leveling"));
        assert!(!is_prose_title("Berserk [Manga]"));
    }

    /// An unmatched bracket is neither a marker nor a reason to stop reading the title.
    #[test]
    fn an_unmatched_bracket_is_stepped_over() {
        assert!(!is_prose_title("Broken [Novel"));
        assert!(is_prose_title("Broken [unclosed (Novel)"));
    }
}
