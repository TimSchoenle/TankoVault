//! What the series page knows about a chapter, assembled from every source that carries it.
//!
//! # Why the merge happens here
//!
//! `GET /v1/series/:id/chapters` answers for **one** source (the first, unless `?source=` says
//! otherwise). The redesigned chapter list needs the opposite shape: one row per chapter,
//! carrying *which* sources have it, how fresh each one is, and where each would open. So the
//! view fetches every source's list concurrently and unions them here.
//!
//! Nothing in this module invents data. A chapter shows the sources that actually returned it;
//! a source that stops at chapter 151 says so because 151 is the highest number it returned.

use crate::models::{ChapterDto, SeriesSourceId, SourceDto};

/// Chapter numbers are decimal with at most a couple of fractional digits (`152.6`), so a
/// fixed-point key is exact for every value the API produces and gives the merge a hashable,
/// orderable identity that `f64` cannot provide.
pub(super) type ChapterKey = i64;

/// Upper bound on a scaled chapter number. Well inside `i64` and exactly representable as an
/// `f64`, so the cast below cannot wrap and cannot lose a digit.
const KEY_CEILING: f64 = 1e15;

/// The fixed-point identity of a chapter number.
pub(super) fn chapter_key(number: f64) -> ChapterKey {
    // Chapter numbers are small positive decimals. Anything outside that range did not come
    // from a chapter, so it collapses to zero rather than wrapping the cast.
    let scaled = (number * 1000.0).round();
    if (0.0..=KEY_CEILING).contains(&scaled) {
        #[allow(clippy::cast_possible_truncation)]
        return scaled as ChapterKey;
    }
    0
}

/// One source's copy of a chapter: where it opens and when that source published it.
#[derive(Clone, PartialEq)]
pub(super) struct Carrier {
    pub(super) source_id: SeriesSourceId,
    pub(super) provider_name: String,
    pub(super) url: String,
    pub(super) published_at: Option<String>,
}

/// A chapter as the reader sees it: one row, every source that carries it, in preference order.
#[derive(Clone, PartialEq)]
pub(super) struct MergedChapter {
    pub(super) number: f64,
    pub(super) title: Option<String>,
    /// Auth-scoped read state; `None` for anonymous readers, who have nothing to track.
    pub(super) read: Option<bool>,
    /// Sources carrying this chapter, highest-ranked first. Never empty.
    pub(super) carriers: Vec<Carrier>,
}

impl MergedChapter {
    /// The source this chapter opens on: the highest-ranked one that actually carries it.
    pub(super) fn resolved(&self) -> &Carrier {
        // `carriers` is only ever built from a non-empty group, so the first element exists.
        &self.carriers[0]
    }

    /// True for a sub-chapter part release (`152.6`), which sources ship ahead of the compiled
    /// whole chapter and which must never count as one.
    pub(super) fn is_part(&self) -> bool {
        self.number.fract() != 0.0
    }
}

/// Rank the series' sources into the order the open control resolves through.
///
/// A per-series pin wins outright; otherwise the API's own `is_primary` flag (the richest
/// source) leads, and the rest fall in by chapter count. Ties break on name so the order is
/// stable across reloads rather than following whatever order the API happened to return.
pub(super) fn rank_sources(
    sources: &[SourceDto],
    pinned: Option<SeriesSourceId>,
) -> Vec<SourceDto> {
    let mut ranked = sources.to_vec();
    ranked.sort_by(|a, b| {
        let key = |s: &SourceDto| {
            (
                // `false` sorts before `true`, so negate the two "should lead" flags.
                pinned != Some(s.id),
                !s.is_primary,
                -i64::from(s.chapter_count),
            )
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| a.provider_name.cmp(&b.provider_name))
    });
    ranked
}

/// Union every source's chapter list into one newest-first list of [`MergedChapter`].
///
/// `per_source` must be in the ranked order produced by [`rank_sources`]: the first source that
/// carries a chapter becomes that chapter's resolved target, and contributes its title.
pub(super) fn merge_chapters(per_source: &[(SourceDto, Vec<ChapterDto>)]) -> Vec<MergedChapter> {
    let mut merged: Vec<MergedChapter> = Vec::new();
    // Position of each chapter key in `merged`, so a second source appends a carrier to the
    // existing row instead of scanning the whole list again.
    let mut index: std::collections::HashMap<ChapterKey, usize> = std::collections::HashMap::new();

    for (source, chapters) in per_source {
        for chapter in chapters {
            let carrier = Carrier {
                source_id: source.id,
                provider_name: source.provider_name.clone(),
                url: chapter.url.clone(),
                published_at: chapter.published_at.clone(),
            };
            if let Some(&at) = index.get(&chapter_key(chapter.number)) {
                let row = &mut merged[at];
                row.carriers.push(carrier);
                // Read state is a property of the reader, not the source, but a source list
                // fetched anonymously carries `None` — take the first definite answer.
                row.read = row.read.or(chapter.read);
                row.title = row
                    .title
                    .take()
                    .or_else(|| non_empty(chapter.title.clone()));
            } else {
                index.insert(chapter_key(chapter.number), merged.len());
                merged.push(MergedChapter {
                    number: chapter.number,
                    title: non_empty(chapter.title.clone()),
                    read: chapter.read,
                    carriers: vec![carrier],
                });
            }
        }
    }

    // Newest first, matching every other chapter surface in the app.
    merged.sort_by(|a, b| b.number.total_cmp(&a.number));
    merged
}

/// The chapter to read next: the lowest-numbered unread one.
///
/// Read state is monotone server-side (`number <= progress`), so "lowest unread" and "first
/// one you have not got to" are the same chapter — no contiguity assumption needed. `None` for
/// an anonymous reader (nothing is tracked) and for a series that is fully read.
pub(super) fn next_unread(chapters: &[MergedChapter]) -> Option<&MergedChapter> {
    chapters
        .iter()
        .filter(|c| c.read == Some(false))
        .min_by(|a, b| a.number.total_cmp(&b.number))
}

/// A source in resolution order, with the ceiling that explains why it may not carry a chapter.
#[derive(Clone, PartialEq)]
pub(super) struct RankedSource {
    pub(super) source: SourceDto,
    /// Highest chapter number this source returned, or `None` when it returned nothing.
    pub(super) ceiling: Option<f64>,
}

/// The highest chapter number a source carries, for the source menu's "only up to ch N".
pub(super) fn source_ceiling(chapters: &[ChapterDto]) -> Option<f64> {
    chapters.iter().map(|c| c.number).max_by(f64::total_cmp)
}

/// A title that is present *and* not blank; a blank title is the same as none.
fn non_empty(title: Option<String>) -> Option<String> {
    title.filter(|t| !t.trim().is_empty())
}

/// One visual row-group, keyed by whole chapter number: the full chapter plus any part
/// releases that share its integer part.
///
/// Until the full chapter appears its parts *are* the reading frontier, so they render
/// directly; once it lands they collapse behind a toggle rather than crowding the list.
#[derive(Clone, PartialEq)]
pub(super) struct ChapterGroup {
    pub(super) full: Option<MergedChapter>,
    /// Part releases in this group, newest (highest) first.
    pub(super) parts: Vec<MergedChapter>,
}

impl ChapterGroup {
    /// The row that represents this group when it is collapsed — the full chapter if there is
    /// one, otherwise its newest part.
    pub(super) fn lead(&self) -> Option<&MergedChapter> {
        self.full.as_ref().or_else(|| self.parts.first())
    }
}

/// Group a newest-first merged list by `floor(number)`.
///
/// Relies on the descending sort, which puts every part release directly above its whole
/// chapter (`152.6` > `152.1` > `152`), so a group's rows are always contiguous.
pub(super) fn group_chapters(list: &[MergedChapter]) -> Vec<ChapterGroup> {
    let mut groups: Vec<(i64, ChapterGroup)> = Vec::new();
    for chapter in list.iter().cloned() {
        // Chapter numbers are small positive counts; the floor of one always fits `i64`.
        #[allow(clippy::cast_possible_truncation)]
        let key = chapter.number.floor() as i64;
        let is_full = !chapter.is_part();
        let slot = match groups.last_mut() {
            Some((k, group)) if *k == key => group,
            _ => {
                groups.push((
                    key,
                    ChapterGroup {
                        full: None,
                        parts: Vec::new(),
                    },
                ));
                // Just pushed, so the last element is the one to fill.
                &mut groups.last_mut().expect("just pushed").1
            }
        };
        if is_full {
            slot.full = Some(chapter);
        } else {
            slot.parts.push(chapter);
        }
    }
    groups.into_iter().map(|(_, group)| group).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(number: f64, title: &str, url: &str) -> ChapterDto {
        ChapterDto {
            number,
            title: Some(title.to_owned()),
            url: url.to_owned(),
            published_at: None,
            read: None,
        }
    }

    /// Deterministic ids, so the tests need neither uuid's `v4` feature nor a random source.
    fn source(name: &str, primary: bool, count: i32, seed: u128) -> SourceDto {
        SourceDto {
            id: SeriesSourceId(uuid::Uuid::from_u128(seed)),
            provider_name: name.to_owned(),
            provider_slug: name.to_lowercase(),
            url: format!("https://{}.example", name.to_lowercase()),
            chapter_count: count,
            is_primary: primary,
        }
    }

    #[test]
    fn chapter_keys_separate_parts_from_whole_chapters() {
        assert_ne!(chapter_key(152.0), chapter_key(152.6));
        assert_eq!(chapter_key(152.6), chapter_key(152.6));
    }

    #[test]
    fn the_pinned_source_outranks_the_primary_one() {
        let primary = source("Asura", true, 158, 1);
        let backup = source("MangaDex", false, 140, 2);
        let ranked = rank_sources(&[primary.clone(), backup.clone()], Some(backup.id));
        assert_eq!(ranked[0].id, backup.id);
        assert_eq!(ranked[1].id, primary.id);
    }

    #[test]
    fn without_a_pin_the_primary_source_leads() {
        let primary = source("Asura", true, 158, 1);
        let backup = source("MangaDex", false, 200, 2);
        let ranked = rank_sources(&[backup.clone(), primary.clone()], None);
        assert_eq!(ranked[0].id, primary.id);
    }

    #[test]
    fn merging_unions_carriers_and_resolves_to_the_highest_ranked() {
        let lead = source("Asura", true, 2, 1);
        let backup = source("MangaDex", false, 3, 2);
        let merged = merge_chapters(&[
            (
                lead.clone(),
                vec![
                    chapter(152.0, "Rope", "https://a/152"),
                    chapter(151.0, "Bell", "https://a/151"),
                ],
            ),
            (
                backup.clone(),
                vec![
                    chapter(152.0, "Rope", "https://m/152"),
                    chapter(151.5, "Part", "https://m/151.5"),
                    chapter(151.0, "Bell", "https://m/151"),
                ],
            ),
        ]);

        assert_eq!(merged.len(), 3);
        assert_eq!(chapter_key(merged[0].number), chapter_key(152.0));
        assert_eq!(merged[0].carriers.len(), 2);
        assert_eq!(merged[0].resolved().url, "https://a/152");
        // The part release only exists on the backup, so that is where it opens.
        assert_eq!(chapter_key(merged[1].number), chapter_key(151.5));
        assert_eq!(merged[1].resolved().provider_name, "MangaDex");
    }

    #[test]
    fn read_state_survives_a_source_that_did_not_report_it() {
        let lead = source("Asura", true, 1, 1);
        let backup = source("MangaDex", false, 1, 2);
        let mut known = chapter(10.0, "T", "https://m/10");
        known.read = Some(true);
        let merged = merge_chapters(&[
            (lead, vec![chapter(10.0, "T", "https://a/10")]),
            (backup, vec![known]),
        ]);
        assert_eq!(merged[0].read, Some(true));
    }

    #[test]
    fn parts_collapse_under_the_whole_chapter_they_belong_to() {
        let lead = source("Asura", true, 4, 1);
        let merged = merge_chapters(&[(
            lead,
            vec![
                chapter(152.0, "a", "u"),
                chapter(151.6, "b", "u"),
                chapter(151.1, "c", "u"),
                chapter(151.0, "d", "u"),
            ],
        )]);
        let groups = group_chapters(&merged);
        assert_eq!(groups.len(), 2);
        assert!(groups[0].parts.is_empty());
        assert_eq!(groups[1].parts.len(), 2);
        assert!(groups[1].full.is_some());
    }

    #[test]
    fn a_group_with_no_full_chapter_leads_with_its_newest_part() {
        let lead = source("Asura", true, 2, 1);
        let merged = merge_chapters(&[(
            lead,
            vec![chapter(152.6, "a", "u"), chapter(152.1, "b", "u")],
        )]);
        let groups = group_chapters(&merged);
        assert!(groups[0].full.is_none());
        assert_eq!(
            groups[0].lead().map(|c| chapter_key(c.number)),
            Some(chapter_key(152.6))
        );
    }

    #[test]
    fn the_ceiling_is_the_highest_number_a_source_returned() {
        let list = vec![chapter(151.0, "a", "u"), chapter(151.6, "b", "u")];
        assert_eq!(
            source_ceiling(&list).map(chapter_key),
            Some(chapter_key(151.6))
        );
        assert_eq!(source_ceiling(&[]).map(chapter_key), None);
    }
}
