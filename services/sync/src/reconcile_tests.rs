//! Reconciliation tests driving [`crate::engine`] through its real entry points against a
//! migrated Postgres and a [`FakeProvider`]. Opt-in: gated behind `integration`, requires Docker.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::OffsetDateTime;

use secrecy::SecretString;
use tankovault_auth::Sealer;
use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{ScannedSeries, SeriesUpsert, ingest_series};
use tankovault_db::repo::providers::{self, NewProvider};
use tankovault_db::repo::{recsys, sync, tracking};
use tankovault_domain::MetadataPriority;
use tankovault_domain::{
    AccountStatus, AdapterKind, ContentType, Politeness, ProviderId, SeriesId, SeriesStatus,
    UserId, WatchStatus, normalize_title,
};
use tankovault_test_support::TestDb;

use crate::engine::SyncEngine;
use crate::mapping::ConflictPolicy;
use crate::provider::{
    ExternalProvider, OAuthTokens, RemoteEntry, RemoteMetadata, RemoteTag, Viewer,
};

/// The provider slug every test in this module links.
const SLUG: &str = "fake";

/// One recorded `save_entry` call — the only remote side effect the engine has.
#[derive(Debug, Clone, PartialEq)]
struct RemoteWrite {
    external_id: String,
    status: WatchStatus,
    progress: f64,
}

/// Shared, observable state behind [`FakeProvider`], so a test can inspect the remote writes
/// after the engine has consumed its `Box<dyn ExternalProvider>`.
#[derive(Default)]
struct FakeState {
    /// What `fetch_list` returns.
    list: Mutex<Vec<RemoteEntry>>,
    /// What `search` resolves any title to.
    search: Mutex<Option<String>>,
    /// Every `save_entry` call, in order.
    writes: Mutex<Vec<RemoteWrite>>,
}

impl FakeState {
    fn writes(&self) -> Vec<RemoteWrite> {
        self.writes.lock().expect("writes mutex").clone()
    }
}

/// A minimal [`ExternalProvider`] whose list, search result and recorded writes a test controls.
struct FakeProvider(Arc<FakeState>);

#[async_trait]
impl ExternalProvider for FakeProvider {
    fn slug(&self) -> &'static str {
        SLUG
    }
    fn display_name(&self) -> &'static str {
        "Fake"
    }
    fn authorize_url(&self) -> String {
        "https://fake.invalid/authorize".to_owned()
    }
    async fn exchange_code(&self, _code: &str) -> anyhow::Result<OAuthTokens> {
        unreachable!("linking goes through the fixture, not an OAuth exchange")
    }
    async fn refresh(&self, _refresh_token: &SecretString) -> anyhow::Result<OAuthTokens> {
        unreachable!("the fixture stores a token that never expires")
    }
    async fn viewer(&self, _access_token: &SecretString) -> anyhow::Result<Viewer> {
        Ok(Viewer {
            id: "1".to_owned(),
            name: "fake-viewer".to_owned(),
        })
    }
    async fn fetch_list(
        &self,
        _access_token: &SecretString,
        _viewer: &Viewer,
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        Ok(self.0.list.lock().expect("list mutex").clone())
    }
    async fn search(
        &self,
        _access_token: &SecretString,
        _title: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(self.0.search.lock().expect("search mutex").clone())
    }
    async fn fetch_public_metadata_by_title(
        &self,
        _title: &str,
    ) -> anyhow::Result<Option<RemoteMetadata>> {
        Ok(None)
    }
    async fn save_entry(
        &self,
        _access_token: &SecretString,
        external_id: &str,
        status: WatchStatus,
        progress: f64,
    ) -> anyhow::Result<()> {
        self.0
            .writes
            .lock()
            .expect("writes mutex")
            .push(RemoteWrite {
                external_id: external_id.to_owned(),
                status,
                progress,
            });
        Ok(())
    }
}

/// A migrated database, a linked account, one local series and the engine under test.
struct Fixture {
    db: TestDb,
    engine: SyncEngine,
    remote: Arc<FakeState>,
    user: UserId,
    series: SeriesId,
    /// The local scraping provider, so a test can re-scrape the series the fixture ingested.
    provider: ProviderId,
}

/// The local series' canonical title. Remote entries reuse it verbatim so the title matcher
/// resolves them with an exact normalized match (similarity 1.0), keeping these tests about
/// merge semantics rather than matcher tuning.
const TITLE: &str = "Solo Leveling";

impl Fixture {
    async fn spawn() -> Self {
        let db = TestDb::spawn().await;
        let user = db.seed_user("reader", &[], AccountStatus::Active).await;

        let provider_id = providers::create(
            &db.pool,
            NewProvider {
                slug: "local-source".to_owned(),
                name: "Local Source".to_owned(),
                base_url: "https://local.invalid".to_owned(),
                adapter: AdapterKind::GenericConfig,
                config: serde_json::json!({}),
                politeness: Politeness::default(),
            },
        )
        .await
        .expect("create local provider")
        .id;

        let series = ingest_series(
            &db.pool,
            &ScannedSeries {
                provider_id,
                source_path: "/manga/solo-leveling".to_owned(),
                provider_title: Some(TITLE.to_owned()),
                meta: SeriesUpsert {
                    canonical_title: TITLE.to_owned(),
                    normalized_title: normalize_title(TITLE),
                    description: None,
                    cover_url: None,
                    content_type: ContentType::Unknown,
                    status: SeriesStatus::Unknown,
                    release_year: None,
                },
                alt_titles: Vec::new(),
                tags: Vec::new(),
                authors: Vec::new(),
                chapters: Vec::new(),
                content_hash: vec![1],
            },
            &MatchingConfig::default(),
            &MetadataPriority::default(),
            &tankovault_domain::TagBlocklist::default(),
            &tankovault_domain::AdultTagSet::defaults(),
        )
        .await
        .expect("ingest local series")
        .series_id;

        let remote = Arc::new(FakeState::default());
        let mut providers_map: HashMap<&'static str, Box<dyn ExternalProvider>> = HashMap::new();
        providers_map.insert(SLUG, Box::new(FakeProvider(remote.clone())));

        let engine = SyncEngine::new(
            db.pool.clone(),
            Sealer::new(&[7u8; 32]),
            ConflictPolicy::NewestWins,
            serde_json::from_value::<MetadataPriority>(serde_json::json!({}))
                .expect("default metadata priority"),
            // Empty: these tests assert what reconciliation writes, and a shipped blocklist term
            // appearing in a fixture would silently change that answer.
            tankovault_domain::TagBlocklist::default(),
            &MatchingConfig::default(),
            providers_map,
        );

        // Link the account by storing a sealed token directly: `link` would go through an OAuth
        // exchange the fake cannot perform, and every test needs the same never-expiring token.
        let sealed = Sealer::new(&[7u8; 32])
            .seal_string(&SecretString::from("access-token"))
            .expect("seal test token");
        sync::upsert_account(&db.pool, user, SLUG, &sealed, None, None)
            .await
            .expect("link fake account");

        Self {
            db,
            engine,
            remote,
            user,
            series,
            provider: provider_id,
        }
    }

    /// Re-scrape the fixture's series from the local adapter, carrying `tags`.
    ///
    /// Same provider and source path, so this converges on the same canonical series rather
    /// than creating a second one.
    async fn rescrape_with_tags(&self, tags: Vec<String>) {
        ingest_series(
            &self.db.pool,
            &ScannedSeries {
                provider_id: self.provider,
                source_path: "/manga/solo-leveling".to_owned(),
                provider_title: Some(TITLE.to_owned()),
                meta: SeriesUpsert {
                    canonical_title: TITLE.to_owned(),
                    normalized_title: normalize_title(TITLE),
                    description: None,
                    cover_url: None,
                    content_type: ContentType::Unknown,
                    status: SeriesStatus::Unknown,
                    release_year: None,
                },
                alt_titles: Vec::new(),
                tags,
                authors: Vec::new(),
                chapters: Vec::new(),
                content_hash: vec![2],
            },
            &MatchingConfig::default(),
            &MetadataPriority::default(),
            &tankovault_domain::TagBlocklist::default(),
            &tankovault_domain::AdultTagSet::defaults(),
        )
        .await
        .expect("re-scrape local series");
    }

    /// The fixture series' tag links and adaptation source, read through the feature
    /// extractor's own query rather than a hand-written one — this is the path the recommender
    /// uses, so it is the path worth asserting on.
    async fn recsys_facts(&self) -> (Vec<(String, f32)>, Option<String>) {
        let facts = recsys::list_series_facts(&self.db.pool, None, 10)
            .await
            .expect("read series facts")
            .into_iter()
            .find(|r| r.series_id == self.series)
            .expect("the fixture series has facts")
            .facts;
        (facts.tags, facts.source)
    }

    /// The appeal signals the prior is computed from, for the fixture series.
    async fn prior_inputs(&self) -> recsys::PriorInputs {
        recsys::prior_inputs_for(&self.db.pool, &[self.series])
            .await
            .expect("read prior inputs")
            .pop()
            .expect("the fixture series has prior inputs")
    }

    /// Put the series on the user's watchlist at `status` with whole-chapter `progress`.
    async fn local_state(&self, status: WatchStatus, progress: f64) {
        tracking::watchlist_upsert(&self.db.pool, self.user, self.series, status, true)
            .await
            .expect("seed watchlist entry");
        tracking::progress_set(&self.db.pool, self.user, self.series, progress)
            .await
            .expect("seed local progress");
    }

    /// Record a common-ancestor snapshot, i.e. pretend a previous reconciliation agreed on
    /// these values. Requires the mapping row, which [`Self::map`] creates.
    async fn snapshot(&self, progress: f64, status: WatchStatus) {
        sync::record_snapshot(
            &self.db.pool,
            &sync::AgreedSnapshot {
                series_id: self.series,
                provider: SLUG,
                local_progress: progress,
                remote_progress: progress,
                local_status: status.as_str(),
                remote_status: status.as_str(),
            },
        )
        .await
        .expect("record snapshot");
    }

    /// Pre-map the series to `external_id` so `resolve_series` short-circuits.
    async fn map(&self, external_id: &str) {
        sync::upsert_mapping(&self.db.pool, self.series, SLUG, external_id)
            .await
            .expect("pre-map series");
    }

    fn set_list(&self, entries: Vec<RemoteEntry>) {
        *self.remote.list.lock().expect("list mutex") = entries;
    }

    async fn local_progress(&self) -> Option<f64> {
        tracking::progress_state(&self.db.pool, self.user, self.series)
            .await
            .expect("read local progress")
            .map(|(p, _)| p)
    }

    async fn local_status(&self) -> Option<WatchStatus> {
        tracking::watchlist_status_get(&self.db.pool, self.user, self.series)
            .await
            .expect("read local status")
    }

    async fn stored_snapshot(&self) -> Option<(Option<f64>, Option<f64>)> {
        sync::get_snapshot(&self.db.pool, self.series, SLUG)
            .await
            .expect("read snapshot")
            .map(|s| (s.last_synced_local_progress, s.last_synced_remote_progress))
    }

    async fn pending_conflicts(&self) -> Vec<(String, String, String)> {
        sync::list_pending_conflicts(&self.db.pool, self.user)
            .await
            .expect("list conflicts")
            .into_iter()
            .map(|c| (c.field, c.local_value, c.remote_value))
            .collect()
    }
}

/// A remote entry for [`TITLE`], `updated_at` seconds after the epoch.
fn remote_entry(
    external_id: &str,
    status: WatchStatus,
    progress: f64,
    updated: i64,
) -> RemoteEntry {
    RemoteEntry {
        status,
        progress,
        updated_at: OffsetDateTime::from_unix_timestamp(updated).expect("valid timestamp"),
        metadata: RemoteMetadata {
            external_id: external_id.to_owned(),
            titles: vec![TITLE.to_owned()],
            ..RemoteMetadata::default()
        },
    }
}

/// Far enough in the past that any locally-written row (`updated_at = now()`) is newer, which is
/// what makes `NewestWins` deterministic without a clock seam.
const STALE: i64 = 1_600_000_000; // 2020-09-13

// Progress values here are small, exactly-representable integers, so exact float comparison is
// correct.
#[expect(
    clippy::float_cmp,
    reason = "asserts the reconciled progress is exactly the value that was pushed"
)]
mod reconcile {
    use super::*;

    #[tokio::test]
    async fn excluded_series_touches_neither_side() {
        // Excluded series must not be read or written by either direction, so a divergence
        // that would otherwise conflict stays divergent.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 12.0).await;
        f.map("m1").await;
        tracking::set_sync_excluded(&f.db.pool, f.user, f.series, true)
            .await
            .expect("exclude from sync");
        f.set_list(vec![remote_entry("m1", WatchStatus::Reading, 99.0, STALE)]);

        let report = f.engine.pull(SLUG, f.user, None).await.expect("pull");

        assert_eq!(report.fetched, 1);
        assert_eq!(report.updated, 0, "no local write for an excluded series");
        assert!(
            f.remote.writes().is_empty(),
            "no remote write for an excluded series"
        );
        assert_eq!(f.local_progress().await, Some(12.0));
    }

    #[tokio::test]
    async fn series_absent_on_the_remote_is_created_from_local_state() {
        // A watchlist entry mapped to a remote id absent from the fetched list is created
        // there outright, local authoritative, seeding a snapshot for the next run.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Paused, 31.0).await;
        f.map("m1").await;
        f.set_list(Vec::new());

        let report = f.engine.push(SLUG, f.user, None).await.expect("push");

        assert_eq!(report.considered, 1);
        assert_eq!(report.pushed, 1);
        assert_eq!(
            f.remote.writes(),
            vec![RemoteWrite {
                external_id: "m1".to_owned(),
                status: WatchStatus::Paused,
                progress: 31.0,
            }]
        );
        assert_eq!(f.stored_snapshot().await, Some((Some(31.0), Some(31.0))));
    }

    #[tokio::test]
    async fn unmapped_local_series_is_reported_not_pushed() {
        // No mapping and no remote search hit: the entry is counted `unmapped` rather than
        // silently pushed against a guessed id.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 4.0).await;
        f.set_list(Vec::new());
        *f.remote.search.lock().expect("search mutex") = None;

        let report = f.engine.push(SLUG, f.user, None).await.expect("push");

        assert_eq!(report.unmapped, 1);
        assert!(f.remote.writes().is_empty());
    }

    #[tokio::test]
    async fn only_the_remote_moved_so_the_local_row_is_pulled() {
        // Ancestor says both sides last agreed at 5. Only the remote advanced, so this is not a
        // conflict and the policy is irrelevant — `LocalWins` must still pull.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 5.0).await;
        f.map("m1").await;
        f.snapshot(5.0, WatchStatus::Reading).await;
        f.set_list(vec![remote_entry("m1", WatchStatus::Reading, 9.0, STALE)]);

        let report = f
            .engine
            .pull(SLUG, f.user, Some(ConflictPolicy::LocalWins))
            .await
            .expect("pull");

        assert_eq!(report.updated, 1);
        assert_eq!(f.local_progress().await, Some(9.0));
        assert!(
            f.remote.writes().is_empty(),
            "a pull must not write to the remote"
        );
        assert_eq!(
            f.stored_snapshot().await,
            Some((Some(9.0), Some(9.0))),
            "the snapshot advances to the agreed value"
        );
    }

    #[tokio::test]
    async fn only_the_local_row_moved_so_one_remote_write_is_issued() {
        // The mirror case, with `RemoteWins`: only the local side changed, so it is not a
        // conflict and the local value is pushed regardless of policy. Exactly one remote write
        // must cover both fields.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 7.0).await;
        f.map("m1").await;
        f.snapshot(5.0, WatchStatus::Reading).await;
        f.set_list(vec![remote_entry("m1", WatchStatus::Reading, 5.0, STALE)]);

        let report = f
            .engine
            .push(SLUG, f.user, Some(ConflictPolicy::RemoteWins))
            .await
            .expect("push");

        assert_eq!(report.pushed, 1);
        assert_eq!(
            f.remote.writes(),
            vec![RemoteWrite {
                external_id: "m1".to_owned(),
                status: WatchStatus::Reading,
                progress: 7.0,
            }],
            "one write, carrying the local progress and the unchanged status"
        );
        assert_eq!(f.local_progress().await, Some(7.0), "local is untouched");
    }

    #[tokio::test]
    async fn ask_me_conflict_queues_and_leaves_the_ancestor_alone() {
        // Under `AskMe`, a genuine conflict must write to neither side and must not advance the
        // ancestor snapshot, or the next run sees "nothing changed" and the conflict is lost.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 7.0).await;
        f.map("m1").await;
        f.snapshot(5.0, WatchStatus::Reading).await;
        f.set_list(vec![remote_entry("m1", WatchStatus::Reading, 9.0, STALE)]);

        let report = f
            .engine
            .pull(SLUG, f.user, Some(ConflictPolicy::AskMe))
            .await
            .expect("pull");

        assert_eq!(report.updated, 0);
        assert!(f.remote.writes().is_empty());
        assert_eq!(f.local_progress().await, Some(7.0));
        assert_eq!(
            f.pending_conflicts().await,
            vec![("progress".to_owned(), "7".to_owned(), "9".to_owned())]
        );
        assert_eq!(
            f.stored_snapshot().await,
            Some((Some(5.0), Some(5.0))),
            "the ancestor must stay at 5 so the conflict is re-detected next run"
        );
    }

    #[tokio::test]
    async fn newest_wins_pulls_when_the_remote_is_the_newer_side() {
        // A genuine conflict under the default policy. The local row was written by
        // `progress_set` just now, so a remote `updated_at` in the future is the only way to
        // make the remote the newer side without a clock seam.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 7.0).await;
        f.map("m1").await;
        f.snapshot(5.0, WatchStatus::Reading).await;
        let future = OffsetDateTime::now_utc().unix_timestamp() + 3600;
        f.set_list(vec![remote_entry("m1", WatchStatus::Reading, 9.0, future)]);

        f.engine
            .pull(SLUG, f.user, Some(ConflictPolicy::NewestWins))
            .await
            .expect("pull");

        assert_eq!(f.local_progress().await, Some(9.0));
        assert!(f.remote.writes().is_empty());
    }

    #[tokio::test]
    async fn newest_wins_pushes_when_the_local_row_is_the_newer_side() {
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 7.0).await;
        f.map("m1").await;
        f.snapshot(5.0, WatchStatus::Reading).await;
        f.set_list(vec![remote_entry("m1", WatchStatus::Reading, 9.0, STALE)]);

        f.engine
            .pull(SLUG, f.user, Some(ConflictPolicy::NewestWins))
            .await
            .expect("pull");

        assert_eq!(f.local_progress().await, Some(7.0));
        assert_eq!(
            f.remote.writes(),
            vec![RemoteWrite {
                external_id: "m1".to_owned(),
                status: WatchStatus::Reading,
                progress: 7.0,
            }]
        );
    }

    #[tokio::test]
    async fn a_first_sync_imports_the_remote_status_without_counting_a_pull_twice() {
        // Imported entries must not also be reported as a pulled status change (double count).
        let f = Fixture::spawn().await;
        f.map("m1").await;
        f.set_list(vec![remote_entry("m1", WatchStatus::Completed, 0.0, STALE)]);

        let report = f.engine.pull(SLUG, f.user, None).await.expect("pull");

        assert_eq!(f.local_status().await, Some(WatchStatus::Completed));
        assert_eq!(
            report.updated, 0,
            "importing the status is not a progress or status pull"
        );
        assert!(f.remote.writes().is_empty(), "both sides already agree");
    }

    #[tokio::test]
    async fn two_remote_ids_resolving_to_one_series_reconcile_it_once() {
        // Two remote works matching one local series must reconcile it once, not replay the
        // merge against a second divergent value — what `handled_series` guards against.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 7.0).await;
        f.set_list(vec![
            remote_entry("dup-a", WatchStatus::Reading, 1.0, STALE),
            remote_entry("dup-b", WatchStatus::Reading, 2.0, STALE),
        ]);

        let report = f.engine.pull(SLUG, f.user, None).await.expect("pull");

        assert_eq!(report.fetched, 2);
        assert_eq!(report.matched, 2, "both entries resolve to the same series");
        let writes = f.remote.writes();
        assert_eq!(writes.len(), 1, "the series is reconciled exactly once");
        assert_eq!(writes[0].progress, 7.0);
    }

    #[tokio::test]
    async fn converged_sides_write_nothing() {
        // Both sides already at the ancestor: a run must be a pure no-op. This is the common
        // case, and a regression here means every scheduled tick writes to the provider.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 5.0).await;
        f.map("m1").await;
        f.snapshot(5.0, WatchStatus::Reading).await;
        f.set_list(vec![remote_entry("m1", WatchStatus::Reading, 5.0, STALE)]);

        let report = f.engine.pull(SLUG, f.user, None).await.expect("pull");

        assert_eq!(report.updated, 0);
        assert!(f.remote.writes().is_empty());
        assert!(f.pending_conflicts().await.is_empty());
    }

    #[tokio::test]
    async fn a_status_only_divergence_pulls_the_remote_status() {
        // Progress agrees; only the status moved on the remote. The status field merges
        // independently of progress, so this must pull the status and issue no remote write.
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 5.0).await;
        f.map("m1").await;
        f.snapshot(5.0, WatchStatus::Reading).await;
        f.set_list(vec![remote_entry("m1", WatchStatus::Dropped, 5.0, STALE)]);

        f.engine.pull(SLUG, f.user, None).await.expect("pull");

        assert_eq!(f.local_status().await, Some(WatchStatus::Dropped));
        assert!(f.remote.writes().is_empty());
    }

    #[tokio::test]
    async fn an_unmatched_remote_entry_is_recorded_but_not_reconciled() {
        // A remote work with no confident local match is still persisted for the admin
        // console's review queue, and counted `unmatched` rather than dropped.
        let f = Fixture::spawn().await;
        let mut entry = remote_entry("m9", WatchStatus::Reading, 3.0, STALE);
        entry.metadata.titles = vec!["Something Entirely Unrelated".to_owned()];
        f.set_list(vec![entry]);

        let report = f.engine.pull(SLUG, f.user, None).await.expect("pull");

        assert_eq!(report.unmatched, 1);
        assert_eq!(report.matched, 0);
        assert!(
            sync::get_remote_entry(&f.db.pool, f.user, SLUG, "m9")
                .await
                .expect("read remote entry")
                .is_some(),
            "the unmatched entry is kept for operator review"
        );
    }
}

/// The widened recommendation signals, from a fetched list entry to the queries the recommender
/// reads them back with.
// Weights are `rank / 100` and IEEE division is correctly rounded, so each is bit-equal to the
// decimal literal asserted against it.
#[expect(
    clippy::float_cmp,
    reason = "asserts a tag weight is exactly the rank the fixture encodes"
)]
mod signals {
    use super::*;

    /// A remote entry carrying every widened signal, on the fixture series' title.
    fn signal_rich_entry(external_id: &str) -> RemoteEntry {
        let mut entry = remote_entry(external_id, WatchStatus::Reading, 5.0, STALE);
        entry.metadata.tags = vec!["Action".to_owned()];
        entry.metadata.themes = vec![
            RemoteTag {
                name: "Regression".to_owned(),
                weight: 0.87,
            },
            RemoteTag {
                name: "Dungeon".to_owned(),
                weight: 0.6,
            },
        ];
        entry.metadata.is_adult = Some(true);
        entry.metadata.external_score = Some(84.0);
        entry.metadata.external_popularity = Some(250_000);
        entry.metadata.external_source = Some("web_novel".to_owned());
        entry
    }

    /// A ranked upstream tag must reach `series_tags.weight` as that rank, and a genre must
    /// reach it at full strength.
    ///
    /// The bug this pins: every column here existed and was read by the model for a whole phase
    /// while nothing wrote any of them, so the recommender's vocabulary silently stayed the ~20
    /// coarse genres and every tag weighed the same. A model reading a column no writer fills
    /// fails as "the recommendations are bland", which points at the ranker, not at the sweep.
    #[tokio::test]
    async fn a_ranked_tag_reaches_the_recommender_with_its_rank_as_the_weight() {
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 5.0).await;
        f.map("m1").await;
        f.set_list(vec![signal_rich_entry("m1")]);

        f.engine.pull(SLUG, f.user, None).await.expect("pull");

        let (mut tags, source) = f.recsys_facts().await;
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            tags,
            vec![
                ("action".to_owned(), 1.0),
                ("dungeon".to_owned(), 0.6),
                ("regression".to_owned(), 0.87),
            ]
        );
        assert_eq!(source.as_deref(), Some("web_novel"));
    }

    /// The adult flag and the appeal priors must reach the series row.
    ///
    /// The bug this pins: `series.is_adult` defaults to `false`, and an unwritten gate is an
    /// open one. This failing means adult series are visible to every reader — in browse,
    /// search and the shelf alike — as a silent failure with no error anywhere.
    ///
    /// Reads through `adult_gated`, the generated column the gate actually tests, rather than
    /// `is_adult` directly: this asserts that what `AniList` said reaches *the column the read
    /// paths consult*, which is the property that matters and the one a refactor could break
    /// while leaving `is_adult` correctly written.
    #[tokio::test]
    async fn the_adult_flag_and_appeal_signals_reach_the_series_row() {
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 5.0).await;
        f.map("m1").await;
        f.set_list(vec![signal_rich_entry("m1")]);

        f.engine.pull(SLUG, f.user, None).await.expect("pull");

        let inputs = f.prior_inputs().await;
        assert!(
            inputs.adult_gated,
            "the adult gate must not stay at its default"
        );
        assert_eq!(inputs.external_score, Some(84.0));
        assert_eq!(inputs.external_popularity, Some(250_000));
    }

    /// A later scrape must not flatten a rank the enrichment path already stored.
    ///
    /// The bug this pins: an adapter knows a tag's name and nothing else, so it writes weight
    /// 1.0. With a plain `DO UPDATE` the two writers alternate on every sweep — the weight flaps
    /// between 0.87 and 1.0, and because the feature digest moves with it, each flap re-embeds
    /// the series for a change that carries no new information.
    #[tokio::test]
    async fn a_later_scrape_does_not_flatten_an_enriched_tag_weight() {
        let f = Fixture::spawn().await;
        f.local_state(WatchStatus::Reading, 5.0).await;
        f.map("m1").await;
        f.set_list(vec![signal_rich_entry("m1")]);
        f.engine.pull(SLUG, f.user, None).await.expect("pull");

        f.rescrape_with_tags(vec!["Regression".to_owned(), "Isekai".to_owned()])
            .await;

        let (tags, _) = f.recsys_facts().await;
        let weight_of = |slug: &str| {
            tags.iter()
                .find(|(s, _)| s == slug)
                .unwrap_or_else(|| panic!("{slug} is missing"))
                .1
        };
        assert_eq!(
            weight_of("regression"),
            0.87,
            "the scrape flattened the rank"
        );
        // A tag only the adapter knows is still added, at the only strength it can state.
        assert_eq!(weight_of("isekai"), 1.0);
    }
}
