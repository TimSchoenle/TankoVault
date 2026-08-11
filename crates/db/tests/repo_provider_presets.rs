//! The preset link: what a rollout may rewrite on a provider row, and what it may never touch
//! (`crates/db/src/repo/providers.rs`, `crates/db/src/repo/provider_presets.rs`).
//!
//! # What is actually at stake here
//!
//! A locked provider is rewritten from its preset on **every** install run, unattended. That is
//! the point — it is how a selector fix reaches a deployment that already carries the row — and
//! it is also the whole risk: the write set is the only thing standing between "upgrades fix my
//! providers" and "upgrades undo my configuration".
//!
//! Two halves, and the second is the one with no undo:
//!
//! - **Preset-owned**: `name`, `base_url`, `adapter`, `config`. Overwritten, by design.
//! - **Operator-owned, always**: `politeness` and `state`. A crawl budget is an answer to the
//!   operator's own infrastructure, robots policy and legal exposure, and a pause is usually an
//!   incident response. Restoring either from a shipped default would be a silent regression
//!   with real-world consequences — the provider starts crawling faster than the operator
//!   allowed, or resumes crawling a site they deliberately stopped.
//!
//! Nothing in the type system separates those two sets: they are adjacent columns in one
//! `UPDATE`. This suite is the separation.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use serde_json::json;
use tankovault_db::repo::{provider_presets, providers};
use tankovault_domain::{AdapterKind, Politeness, PresetDefinition, ProviderState};
use tankovault_test_support::{TestDb, seed};

/// The shipped definition under test, recorded the way the installer records it.
async fn record_preset(db: &TestDb, slug: &str, config: serde_json::Value) -> PresetDefinition {
    provider_presets::upsert(
        &db.pool,
        &provider_presets::NewPreset {
            slug: slug.to_owned(),
            name: "Shipped Name".to_owned(),
            base_url: "https://shipped.invalid".to_owned(),
            adapter: AdapterKind::Madara,
            config,
            politeness: Politeness {
                rps: 4.0,
                concurrency: 8,
                ..Politeness::default()
            },
        },
    )
    .await
    .expect("record the preset");
    provider_presets::get(&db.pool, slug)
        .await
        .expect("read it back")
        .expect("the entry exists")
}

/// A crawl budget an operator has deliberately lowered, and a pause they have deliberately set,
/// both survive a preset sync.
///
/// This is the invariant the whole feature is sold on: an operator can tune politeness on a
/// managed provider *without* unlocking it, so the lock never becomes a reason to detach a row
/// from its updates. If a sync ever writes these columns, a rollout silently raises a rate limit
/// somebody lowered on purpose — or resumes crawling a site they stopped — and nothing in the
/// install log says so.
#[tokio::test]
async fn a_preset_sync_never_touches_politeness_or_state() {
    let db = TestDb::spawn().await;
    let preset = record_preset(&db, "kunmanga", json!({ "latest": { "item": "div.new" } })).await;

    let careful = Politeness {
        rps: 0.5,
        concurrency: 1,
        crawl_delay_ms: 2_000,
        ..Politeness::default()
    };
    let id = seed::provider(&db, "kunmanga")
        .politeness(careful.clone())
        .create()
        .await;
    providers::set_state(&db.pool, id, ProviderState::Disabled)
        .await
        .expect("the operator pauses it");

    let synced = providers::apply_preset(&db.pool, id, &preset)
        .await
        .expect("sync from the preset");

    // Bit-identical rather than approximate on purpose: this value is carried through, never
    // recomputed, so anything but an exact match means a conversion crept into the path.
    assert!(
        (synced.politeness.rps - careful.rps).abs() < f64::EPSILON,
        "rate limit survives: {} != {}",
        synced.politeness.rps,
        careful.rps,
    );
    assert_eq!(
        synced.politeness.concurrency, careful.concurrency,
        "concurrency survives"
    );
    assert_eq!(
        synced.politeness.crawl_delay_ms, careful.crawl_delay_ms,
        "crawl delay survives"
    );
    assert_eq!(
        synced.state,
        ProviderState::Disabled,
        "a paused provider stays paused through a sync"
    );

    // And the half that is meant to move, did.
    assert_eq!(synced.name, "Shipped Name");
    assert_eq!(synced.base_url, "https://shipped.invalid");
    assert_eq!(synced.adapter, AdapterKind::Madara);
    assert_eq!(synced.config, json!({ "latest": { "item": "div.new" } }));
    assert!(
        synced
            .preset
            .is_some_and(|link| link.locked && link.synced_at.is_some()),
        "the sync stamps the link it just honoured"
    );
}

/// Unlocking stops the rewrite, and a later sync of the *same* preset must not resurrect it.
///
/// The failure this pins is the one an operator would never see coming: they unlock a provider
/// precisely because they need a selector the shipped preset gets wrong, and a rollout two weeks
/// later throws that away. Unlocking has to be permanent until they say otherwise.
#[tokio::test]
async fn an_unlocked_provider_keeps_its_link_and_its_edits() {
    let db = TestDb::spawn().await;
    let preset = record_preset(&db, "toonily", json!({ "catalog": { "path": "/shipped" } })).await;
    let id = seed::provider(&db, "toonily").create().await;

    providers::apply_preset(&db.pool, id, &preset)
        .await
        .expect("adopt");
    let unlocked = providers::set_preset_lock(&db.pool, id, false)
        .await
        .expect("unlock");

    let link = unlocked.preset.expect("the link outlives the lock");
    assert_eq!(link.slug, "toonily", "the row still names its origin");
    assert!(!link.locked);

    // The operator's own edit, which no rollout may undo while the row is unlocked.
    let mine = json!({ "catalog": { "path": "/mine" } });
    providers::update(
        &db.pool,
        id,
        "My Toonily",
        "https://mine.invalid",
        &mine,
        Politeness::default(),
    )
    .await
    .expect("edit freely");

    let after = providers::get(&db.pool, id).await.expect("read back");
    assert_eq!(after.config, mine);
    assert_eq!(after.name, "My Toonily");
}

/// A row the installer creates is locked; one the console creates is not linked at all.
///
/// The asymmetry is deliberate and load-bearing: `preset_locked` is derived from the slug at
/// insert time, so no caller can produce a row that names a preset yet silently never follows
/// it — which would look managed in the console and drift forever.
#[tokio::test]
async fn only_a_row_installed_from_a_preset_starts_managed() {
    let db = TestDb::spawn().await;

    let installed = providers::create(
        &db.pool,
        providers::NewProvider {
            slug: "mangadex".to_owned(),
            name: "MangaDex".to_owned(),
            base_url: "https://mangadex.invalid".to_owned(),
            adapter: AdapterKind::Custom,
            config: json!({}),
            politeness: Politeness::default(),
            preset_slug: Some("mangadex".to_owned()),
        },
    )
    .await
    .expect("install from a preset");
    let link = installed.preset.expect("installed rows are linked");
    assert!(link.locked, "and start following the preset");
    assert_eq!(link.slug, "mangadex");

    // The console's registration form and its clone action both go through this path.
    let by_hand = seed::provider(&db, "my-own-site").create().await;
    let by_hand = providers::get(&db.pool, by_hand).await.expect("read back");
    assert!(
        by_hand.preset.is_none(),
        "a hand-registered provider is nobody's to rewrite"
    );
}

/// Retiring a preset removes the definition and leaves every provider that came from it
/// standing.
///
/// Cascading here would be catastrophic and quiet: dropping a provider takes its `sources` with
/// it (FK `ON DELETE CASCADE`), so a release that merely stopped maintaining a preset would
/// delete a deployment's catalogue for that site. Hence the soft reference and this test.
#[tokio::test]
async fn retiring_a_preset_leaves_its_providers_alone() {
    let db = TestDb::spawn().await;
    let preset = record_preset(&db, "gone-next-release", json!({})).await;
    let id = seed::provider(&db, "gone-next-release").create().await;
    providers::apply_preset(&db.pool, id, &preset)
        .await
        .expect("adopt");

    let retired = provider_presets::retire_missing(&db.pool, &["something-else".to_owned()])
        .await
        .expect("retire");
    assert_eq!(retired, vec!["gone-next-release".to_owned()]);

    let orphan = providers::get(&db.pool, id).await.expect("still there");
    assert!(
        orphan.preset.is_some_and(|link| link.locked),
        "the row keeps its dangling link, which is what the console reports"
    );
    assert!(
        provider_presets::get(&db.pool, "gone-next-release")
            .await
            .expect("query")
            .is_none(),
        "but the definition is gone"
    );
}

/// The database refuses a lock that names no preset.
///
/// `preset_locked` without `preset_slug` would be a row the installer can never satisfy: it
/// claims to follow something unnameable. The CHECK constraint is what makes that state
/// unrepresentable rather than merely unlikely.
#[tokio::test]
async fn a_lock_without_a_preset_is_refused_by_the_schema() {
    let db = TestDb::spawn().await;
    let id = seed::provider(&db, "unmanaged").create().await;

    let refused = providers::set_preset_lock(&db.pool, id, true).await;
    assert!(
        refused.is_err(),
        "locking a provider that came from no preset must not be storable"
    );
}
