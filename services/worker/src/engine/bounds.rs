//! Length ceilings on scraped text, applied once at the boundary between an adapter's output
//! and the ingest transaction.
//!
//! No text column in the schema is `varchar(n)` — they are all `text`, deliberately, because a
//! provider is free to relabel a series and a length that fits today's catalogue would start
//! refusing ingests tomorrow. That leaves the crawler as the only place a ceiling can live, and
//! until now there was none: the sole bound on a scraped title was `MAX_BODY_BYTES`, the 16 MiB
//! whole-response cap in `tankovault_fetch`. A page whose title selector matched a container
//! instead of a heading therefore wrote megabytes into `series.canonical_title`, and from there
//! into the `gin_trgm_ops` indexes migration 0033 built over it — where a megabyte of text is
//! roughly a million trigram entries in an index every catalogue search reads.
//!
//! Truncation, not rejection. A title that got mis-scraped is still the only handle the operator
//! has on that source, and dropping the series would lose the row that names what went wrong.

use tankovault_adapters::{ChapterMeta, SeriesMeta};

/// A series title, its alternates, and a provider's label for a source.
pub(crate) const MAX_TITLE: usize = 512;
/// A synopsis. Long-form by nature, so this is generous; it is a ceiling, not a target.
pub(crate) const MAX_DESCRIPTION: usize = 8192;
/// A tag or a credit. Both are indexed vocabulary, and the blocklist that prunes them works on
/// terms, not essays.
pub(crate) const MAX_TERM: usize = 128;
/// A URL. Browsers stop at about 2 000 characters and no provider comes near it.
pub(crate) const MAX_URL: usize = 2048;
/// A chapter path. The largest per-row text in `chapters`, the biggest table in the deployment.
pub(crate) const MAX_PATH: usize = 1024;
/// A chapter title.
pub(crate) const MAX_CHAPTER_TITLE: usize = 512;
/// Most entries a single listing may contribute to one of the vocabulary tables. A page that
/// yields more tags than this matched the wrong container, not a well-tagged series.
pub(crate) const MAX_TERMS: usize = 64;
/// Alternative titles kept for one series. Each one is a row and a trigram-indexed string.
pub(crate) const MAX_ALT_TITLES: usize = 32;

/// Truncate to at most `max` **bytes**, on a character boundary.
///
/// Byte-bounded rather than character-bounded because the storage cost this exists to bound is
/// bytes; `floor_char_boundary` keeps the result valid UTF-8 either way.
fn clip(value: &mut String, max: usize) {
    if value.len() > max {
        value.truncate(value.floor_char_boundary(max));
    }
}

fn clip_opt(value: &mut Option<String>, max: usize) {
    if let Some(v) = value {
        clip(v, max);
    }
}

fn clip_all(values: &mut Vec<String>, max_each: usize, max_count: usize) {
    values.truncate(max_count);
    for v in values.iter_mut() {
        clip(v, max_each);
    }
}

/// Bound every scraped text field of a series to its ceiling, in place.
pub(crate) fn bound_series(meta: &mut SeriesMeta) {
    clip(&mut meta.title, MAX_TITLE);
    clip_all(&mut meta.alt_titles, MAX_TITLE, MAX_ALT_TITLES);
    clip_opt(&mut meta.description, MAX_DESCRIPTION);
    clip_opt(&mut meta.cover_url, MAX_URL);
    clip_all(&mut meta.tags, MAX_TERM, MAX_TERMS);
    clip_all(&mut meta.authors, MAX_TERM, MAX_TERMS);
}

/// Bound every scraped text field of a chapter listing, in place.
///
/// The entry count is deliberately *not* bounded here: a listing legitimately runs to thousands
/// of chapters, and which of them are real is [`super::drop_implausible`]'s question, not this
/// module's.
pub(crate) fn bound_chapters(chapters: &mut [ChapterMeta]) {
    for c in chapters {
        clip_opt(&mut c.title, MAX_CHAPTER_TITLE);
        clip(&mut c.path, MAX_PATH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::{ContentType, SeriesStatus};

    fn meta(title: &str) -> SeriesMeta {
        SeriesMeta {
            title: title.to_owned(),
            alt_titles: Vec::new(),
            description: None,
            cover_url: None,
            tags: Vec::new(),
            authors: Vec::new(),
            status: SeriesStatus::Ongoing,
            content_type: ContentType::Manga,
            release_year: None,
        }
    }

    /// The bug this bounds: a title selector that matched a page container rather than the
    /// heading wrote the whole page into `canonical_title`, and every catalogue search then read
    /// the trigram index that string had exploded.
    #[test]
    fn a_runaway_title_is_clipped() {
        let mut m = meta(&"x".repeat(1_000_000));
        bound_series(&mut m);
        assert_eq!(m.title.len(), MAX_TITLE);
    }

    /// Truncation must not split a multi-byte character — a `String` that is not valid UTF-8 is
    /// not representable, so getting this wrong is a panic in the scan path.
    #[test]
    fn truncation_lands_on_a_character_boundary() {
        // Every character is 3 bytes, so MAX_TITLE (512) falls mid-character.
        let mut m = meta(&"あ".repeat(1000));
        bound_series(&mut m);
        assert!(m.title.len() <= MAX_TITLE);
        assert_eq!(m.title.len() % 3, 0, "clipped mid-character");
    }

    #[test]
    fn text_inside_the_ceiling_is_untouched() {
        let mut m = meta("Solo Leveling");
        bound_series(&mut m);
        assert_eq!(m.title, "Solo Leveling");
    }

    #[test]
    fn a_page_that_yielded_a_thousand_tags_keeps_only_the_cap() {
        let mut m = meta("x");
        m.tags = (0..1000).map(|i| format!("tag-{i}")).collect();
        bound_series(&mut m);
        assert_eq!(m.tags.len(), MAX_TERMS);
    }
}
