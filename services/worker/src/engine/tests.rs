//! Engine unit tests.

use super::*;
use tankovault_domain::{AdapterKind, Politeness, ProviderState};
use time::OffsetDateTime;

fn provider(rps: f64, ua: &str) -> Provider {
    Provider {
        id: ProviderId::from_uuid(uuid::Uuid::nil()),
        slug: "demo".to_owned(),
        name: "Demo".to_owned(),
        base_url: "https://demo.test".to_owned(),
        adapter: AdapterKind::Madara,
        config: serde_json::json!({}),
        state: ProviderState::Active,
        politeness: Politeness {
            rps,
            concurrency: 2,
            crawl_delay_ms: 0,
            user_agent: ua.to_owned(),
            emulation: None,
        },
        last_full_scan_at: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

/// A provider nobody has tuned crawls as the deployment, not as this project; a provider with
/// an explicit user-agent keeps it, because that is a decision about how *that* site is
/// approached and it outranks the deployment's generic identity.
#[test]
fn the_configured_crawler_identity_replaces_only_the_shipped_default() {
    let branded = Some("MangaBoxBot/1.0 (+https://mangabox.example)");
    assert_eq!(
        crawl_identity(tankovault_domain::politeness::DEFAULT_USER_AGENT, branded),
        "MangaBoxBot/1.0 (+https://mangabox.example)"
    );
    assert_eq!(
        crawl_identity("SiteSpecific/2.0", branded),
        "SiteSpecific/2.0"
    );
    assert_eq!(
        crawl_identity(tankovault_domain::politeness::DEFAULT_USER_AGENT, None),
        tankovault_domain::politeness::DEFAULT_USER_AGENT
    );
}

/// The cache key must change when — and only when — a setting the fetch stack is built
/// from changes. Too eager and every task rebuilds again (the bug this replaced); too
/// lazy and an operator lowering `rps` mid-run is ignored until the process restarts.
#[test]
fn the_fingerprint_tracks_exactly_the_settings_the_stack_is_built_from() {
    let base = provider(1.0, "tankovault/1.0");
    assert_eq!(
        politeness_fingerprint(&base),
        politeness_fingerprint(&provider(1.0, "tankovault/1.0")),
        "identical politeness must reuse the stack, or the limiter is per-task again"
    );

    assert_ne!(
        politeness_fingerprint(&base),
        politeness_fingerprint(&provider(0.5, "tankovault/1.0")),
        "lowering rps must take effect without a restart"
    );
    assert_ne!(
        politeness_fingerprint(&base),
        politeness_fingerprint(&provider(1.0, "other/1.0"))
    );

    // Fields the fetch stack does not read must NOT invalidate a warm connection pool,
    // a rate limiter and an accumulated throttle penalty.
    let mut renamed = provider(1.0, "tankovault/1.0");
    renamed.name = "Renamed".to_owned();
    renamed.config = serde_json::json!({ "unrelated": true });
    renamed.state = ProviderState::Degraded;
    assert_eq!(
        politeness_fingerprint(&base),
        politeness_fingerprint(&renamed),
        "an unrelated column change must not throw away the fetch stack"
    );
}

/// `rps` is an `f64`; hashing it at all requires going through the bit pattern.
#[test]
fn fractional_rates_are_distinguished() {
    assert_ne!(
        politeness_fingerprint(&provider(0.5, "ua")),
        politeness_fingerprint(&provider(0.25, "ua"))
    );
}

fn meta(title: &str, description: Option<&str>) -> SeriesMeta {
    SeriesMeta {
        title: title.to_owned(),
        alt_titles: Vec::new(),
        description: description.map(str::to_owned),
        cover_url: None,
        tags: Vec::new(),
        authors: Vec::new(),
        status: tankovault_domain::SeriesStatus::Ongoing,
        content_type: tankovault_domain::ContentType::Manga,
        release_year: Some(2020),
    }
}

fn chapter(number: f64, title: Option<&str>, path: &str) -> ChapterMeta {
    ChapterMeta {
        number,
        title: title.map(str::to_owned),
        path: path.to_owned(),
        published_at: None,
        access: tankovault_adapters::ChapterAccess::Free,
    }
}

/// The listing is in the source's order, which is usually newest-first — so the stray is at
/// index 0 while the domain rule reasons about it sorted. Getting the index bookkeeping
/// wrong here deletes real chapters and keeps the junk.
#[test]
fn rejecting_a_stray_removes_that_entry_and_no_other() {
    let mut chapters = vec![chapter(9000.0, None, "/manga/x/chapter-9000/")];
    chapters.extend((1..=40).map(|n| chapter(f64::from(n), None, "/manga/x/")));

    drop_implausible(
        &OutlierPolicy::default(),
        &provider(1.0, "ua"),
        "/manga/x",
        &mut chapters,
    );

    assert_eq!(chapters.len(), 40);
    assert!(
        chapters.iter().all(|c| c.number <= 40.0),
        "the stray survived: {:?}",
        chapters.iter().map(|c| c.number).collect::<Vec<_>>()
    );
}

#[test]
fn a_plausible_listing_is_left_untouched() {
    let mut chapters: Vec<ChapterMeta> = (1..=40)
        .map(|n| chapter(f64::from(n), None, "/manga/x/"))
        .collect();
    let before = chapters.len();

    drop_implausible(
        &OutlierPolicy::default(),
        &provider(1.0, "ua"),
        "/manga/x",
        &mut chapters,
    );

    assert_eq!(chapters.len(), before);
}

/// Rejection happens before the hash, so a source that keeps serving the same junk hashes
/// as unchanged. Hashing first would make every re-scan of such a series look changed and
/// re-do the full ingest forever.
#[test]
fn a_rejected_chapter_does_not_move_the_content_hash() {
    let series = meta("Solo Leveling", None);
    let listing = || -> Vec<ChapterMeta> {
        (1..=40)
            .map(|n| chapter(f64::from(n), None, "/manga/x/"))
            .collect()
    };
    let clean = listing();

    let mut with_junk = listing();
    with_junk.push(chapter(9000.0, None, "/manga/x/chapter-9000/"));
    drop_implausible(
        &OutlierPolicy::default(),
        &provider(1.0, "ua"),
        "/manga/x",
        &mut with_junk,
    );

    assert_eq!(
        content_hash(&series, &clean),
        content_hash(&series, &with_junk)
    );
}

/// Determinism is the entire contract: a hash that varies for identical input makes
/// every scan look changed (wasteful); one that's stable across a real change stops
/// updates for that series silently — the failure nobody notices.
#[test]
fn the_content_hash_is_deterministic_for_identical_input() {
    let chapters = vec![
        chapter(1.0, Some("Awakening"), "/manga/x/1/"),
        chapter(2.0, None, "/manga/x/2/"),
    ];
    assert_eq!(
        content_hash(&meta("Solo Leveling", Some("blurb")), &chapters),
        content_hash(&meta("Solo Leveling", Some("blurb")), &chapters),
    );
}

/// Every field the hash is documented to cover must actually change it.
#[test]
fn the_content_hash_changes_when_a_covered_field_changes() {
    let base_meta = meta("Solo Leveling", Some("blurb"));
    let base = vec![chapter(1.0, None, "/manga/x/1/")];
    let baseline = content_hash(&base_meta, &base);

    assert_ne!(
        baseline,
        content_hash(&meta("Solo Levelling", Some("blurb")), &base),
        "a retitled series must be seen as changed"
    );
    assert_ne!(
        baseline,
        content_hash(&meta("Solo Leveling", Some("rewritten")), &base),
        "a rewritten description must be seen as changed"
    );
    assert_ne!(
        baseline,
        content_hash(&base_meta, &[chapter(1.5, None, "/manga/x/1/")]),
        "a renumbered chapter must be seen as changed"
    );
    assert_ne!(
        baseline,
        content_hash(&base_meta, &[chapter(1.0, None, "/manga/x/1-v2/")]),
        "a relinked chapter must be seen as changed"
    );
    assert_ne!(
        baseline,
        content_hash(
            &base_meta,
            &[
                chapter(1.0, None, "/manga/x/1/"),
                chapter(2.0, None, "/manga/x/2/"),
            ]
        ),
        "a new chapter must be seen as changed — this is the case the whole scan exists for"
    );
}

/// Two things the hash deliberately does *not* cover.
///
/// 1. Chapter titles aren't hashed — a chapter retitled in place reports "no change",
///    intentional since scanlation sites edit labels constantly.
/// 2. Chapter order is significant — a reordered listing reports a change, costing
///    work but never wrong; an order-insensitive hash would be a behaviour change.
#[test]
fn the_content_hash_ignores_chapter_titles_and_respects_chapter_order() {
    let series = meta("Solo Leveling", None);
    let untitled = vec![chapter(1.0, None, "/manga/x/1/")];
    let titled = vec![chapter(1.0, Some("Awakening"), "/manga/x/1/")];
    assert_eq!(
        content_hash(&series, &untitled),
        content_hash(&series, &titled),
        "chapter titles are outside the hash; if that changes, change this test on purpose"
    );

    let ascending = vec![
        chapter(1.0, None, "/manga/x/1/"),
        chapter(2.0, None, "/manga/x/2/"),
    ];
    let descending = vec![
        chapter(2.0, None, "/manga/x/2/"),
        chapter(1.0, None, "/manga/x/1/"),
    ];
    assert_ne!(
        content_hash(&series, &ascending),
        content_hash(&series, &descending),
        "the hash is order-sensitive today; making it order-insensitive is a behaviour \
             change, not a cleanup"
    );
}

/// A chapter path can't be made to look like a different chapter list by embedding the
/// framing bytes (`number | path \n`) the hash uses to separate entries — a classic
/// collision providers could otherwise force.
#[test]
fn a_chapter_path_carrying_the_separator_bytes_does_not_forge_another_chapter() {
    let series = meta("X", None);
    let smuggled = vec![chapter(1.0, None, "/a/\n\u{0}|/b/")];
    let genuine = vec![chapter(1.0, None, "/a/"), chapter(1.0, None, "/b/")];
    assert_ne!(
        content_hash(&series, &smuggled),
        content_hash(&series, &genuine),
        "a provider-supplied path must not be able to impersonate a second chapter"
    );
}

/// The pairing key must agree with the tolerance comparison it replaced, on every
/// number a chapter can carry — disagreement loses notifications silently.
#[test]
fn the_pairing_key_agrees_with_the_tolerance_it_replaced() {
    let numbers = [
        0.0, 1.0, 1.5, 2.0, 3.5, 152.0, 152.1, 152.5, 152.6, 153.0, 9999.0, 0.001, 1e9,
    ];
    for &a in &numbers {
        for &b in &numbers {
            let by_key = chapter_key(a) == chapter_key(b);
            let by_tolerance = (a - b).abs() < f64::EPSILON;
            assert_eq!(
                by_key, by_tolerance,
                "`{a}` vs `{b}`: the hash key and the old comparison must decide the same                      way, or a chapter is announced twice or not at all"
            );
        }
    }
}

/// Chapter 0 is real — a prologue — and it is the one value where a bit pattern and a
/// tolerance would disagree, because `-0.0` and `0.0` are equal numbers with different
/// bits. Normalising it is what keeps a notification from depending on a sign bit.
#[test]
fn negative_zero_is_the_same_chapter_as_zero() {
    assert_eq!(chapter_key(-0.0), chapter_key(0.0));
}
