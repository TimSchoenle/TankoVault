//! Entity builders for the `crates/db` suites (TEST F-09).
//!
//! Use these instead of writing another one-off `a_provider`/`a_user`/`a_series` fixture:
//! duplicated fixtures drift silently (`repo_ingest.rs`'s copy once diverged on adapter and base
//! URL with nothing to say whether that was deliberate), and a builder keeps the common case one
//! line while making the uncommon case explicit at the call site:
//!
//! ```ignore
//! let provider = seed::provider(&db, "alpha").create().await;
//! let madara = seed::provider(&db, "beta").adapter(AdapterKind::Madara).create().await;
//! let series = seed::series(&db, provider, "Berserk").chapters(&[1.0, 2.0]).create().await;
//! ```
//!
//! Each terminal `create` calls the same `repo::` entry point the suite would call by hand, so
//! seeding through a builder still exercises the real write path rather than a hand-rolled
//! insert that could pass while the repository is broken. Every failure here panics — it is a
//! broken fixture, not a behaviour under test.

use crate::TestDb;
use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{
    ChapterUpsert, IngestOutcome, ScannedSeries, SeriesUpsert, ingest_series,
};
use tankovault_db::repo::providers::{self, NewProvider};
use tankovault_db::repo::users;
use tankovault_domain::{
    AdapterKind, ContentType, Politeness, ProviderId, SeriesId, SeriesStatus, UserId,
    normalize_title,
};

/// Start building a provider identified by `slug`.
///
/// Defaults: the slug doubles as the display name, the base URL is `https://{slug}.invalid` —
/// a reserved TLD, so a fixture that leaks into a real fetch fails to resolve rather than
/// reaching somebody's site — the adapter is [`AdapterKind::GenericConfig`], and the politeness
/// is the default.
#[must_use]
pub fn provider<'a>(db: &'a TestDb, slug: &'a str) -> ProviderBuilder<'a> {
    ProviderBuilder {
        db,
        slug,
        name: None,
        base_url: None,
        adapter: AdapterKind::GenericConfig,
        config: serde_json::Value::Object(serde_json::Map::new()),
        politeness: Politeness::default(),
    }
}

/// A provider row waiting to be written. See [`provider`].
pub struct ProviderBuilder<'a> {
    db: &'a TestDb,
    slug: &'a str,
    name: Option<&'a str>,
    base_url: Option<String>,
    adapter: AdapterKind,
    config: serde_json::Value,
    politeness: Politeness,
}

impl<'a> ProviderBuilder<'a> {
    /// Override the display name, which otherwise repeats the slug.
    #[must_use]
    pub fn name(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }

    /// Override the base URL. Only worth doing when the test asserts something *about* the URL —
    /// link resolution, or the SSRF guard — since the default is already unreachable.
    #[must_use]
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Override the adapter kind.
    #[must_use]
    pub fn adapter(mut self, adapter: AdapterKind) -> Self {
        self.adapter = adapter;
        self
    }

    /// Override the adapter configuration blob.
    #[must_use]
    pub fn config(mut self, config: serde_json::Value) -> Self {
        self.config = config;
        self
    }

    /// Override the crawl politeness.
    #[must_use]
    pub fn politeness(mut self, politeness: Politeness) -> Self {
        self.politeness = politeness;
        self
    }

    /// Write the row through `repo::providers::create` and return its id.
    ///
    /// # Panics
    /// If the insert fails, which in a test always means the fixture is wrong.
    pub async fn create(self) -> ProviderId {
        let slug = self.slug;
        providers::create(
            &self.db.pool,
            NewProvider {
                slug: slug.to_owned(),
                name: self.name.unwrap_or(slug).to_owned(),
                base_url: self
                    .base_url
                    .unwrap_or_else(|| format!("https://{slug}.invalid")),
                adapter: self.adapter,
                config: self.config,
                politeness: self.politeness,
            },
        )
        .await
        .expect("seed provider")
        .id
    }
}

/// Start building a user identified by `username`.
///
/// The email is derived as `{username}@example.test` and the password hash is a placeholder,
/// matching [`TestDb::seed_user`] — which remains the one to reach for when the test needs
/// capabilities or a non-active account status, since that is what it exists to set.
#[must_use]
pub fn user<'a>(db: &'a TestDb, username: &'a str) -> UserBuilder<'a> {
    UserBuilder {
        db,
        username,
        email: None,
    }
}

/// A user row waiting to be written. See [`user`].
pub struct UserBuilder<'a> {
    db: &'a TestDb,
    username: &'a str,
    email: Option<&'a str>,
}

impl<'a> UserBuilder<'a> {
    /// Override the derived email address.
    ///
    /// Worth doing for exactly one family of test: `users.email` is `citext`, and F-05c was a
    /// total lockout caused by comparing it case-sensitively, so a suite that pins case
    /// insensitivity has to control the stored casing rather than take it from a username.
    #[must_use]
    pub fn email(mut self, email: &'a str) -> Self {
        self.email = Some(email);
        self
    }

    /// Write the row through `repo::users::create` and return its id.
    ///
    /// # Panics
    /// If the insert fails, which in a test always means the fixture is wrong.
    pub async fn create(self) -> UserId {
        let username = self.username;
        let email = self
            .email
            .map_or_else(|| format!("{username}@example.test"), ToOwned::to_owned);
        users::create(&self.db.pool, &email, username, "$argon2id$seed")
            .await
            .expect("seed user")
            .id
    }
}

/// Start building a canonical series carried by `provider_id`.
///
/// The normalized title is derived with the real [`normalize_title`], and the source path from
/// it, so two series with different titles never collide on `(provider_id, source_path)` and a
/// test that seeds the same title twice gets the idempotent behaviour the ingest path really has.
#[must_use]
pub fn series<'a>(db: &'a TestDb, provider_id: ProviderId, title: &'a str) -> SeriesBuilder<'a> {
    SeriesBuilder {
        db,
        provider_id,
        title,
        source_path: None,
        chapters: &[],
        alt_titles: Vec::new(),
        tags: Vec::new(),
        authors: Vec::new(),
        content_type: ContentType::Manga,
        status: SeriesStatus::Ongoing,
        release_year: None,
        content_hash: vec![1],
    }
}

/// A scanned series waiting to be ingested. See [`series`].
pub struct SeriesBuilder<'a> {
    db: &'a TestDb,
    provider_id: ProviderId,
    title: &'a str,
    source_path: Option<String>,
    chapters: &'a [f64],
    alt_titles: Vec<(String, String)>,
    tags: Vec<String>,
    authors: Vec<String>,
    content_type: ContentType,
    status: SeriesStatus,
    release_year: Option<i32>,
    content_hash: Vec<u8>,
}

impl<'a> SeriesBuilder<'a> {
    /// The chapter numbers to ingest. Each becomes a chapter at `/c/{number}`.
    #[must_use]
    pub fn chapters(mut self, chapters: &'a [f64]) -> Self {
        self.chapters = chapters;
        self
    }

    /// Override the source path, which otherwise derives from the normalized title.
    #[must_use]
    pub fn source_path(mut self, source_path: impl Into<String>) -> Self {
        self.source_path = Some(source_path.into());
        self
    }

    /// Alternative titles, normalized with the real [`normalize_title`] so the search and
    /// matching paths see what production would.
    #[must_use]
    pub fn alt_titles(mut self, titles: &[&str]) -> Self {
        self.alt_titles = titles
            .iter()
            .map(|t| ((*t).to_owned(), normalize_title(t)))
            .collect();
        self
    }

    /// Genre/tag names.
    #[must_use]
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|t| (*t).to_owned()).collect();
        self
    }

    /// Author/artist credits.
    #[must_use]
    pub fn authors(mut self, authors: &[&str]) -> Self {
        self.authors = authors.iter().map(|a| (*a).to_owned()).collect();
        self
    }

    /// Override the content type, which defaults to [`ContentType::Manga`].
    #[must_use]
    pub fn content_type(mut self, content_type: ContentType) -> Self {
        self.content_type = content_type;
        self
    }

    /// Override the publication status, which defaults to [`SeriesStatus::Ongoing`].
    #[must_use]
    pub fn status(mut self, status: SeriesStatus) -> Self {
        self.status = status;
        self
    }

    /// Set the release year, which defaults to absent.
    #[must_use]
    pub fn release_year(mut self, year: i32) -> Self {
        self.release_year = Some(year);
        self
    }

    /// Set the release year from a value that may already be absent.
    ///
    /// Exists so a table-driven fixture whose rows carry `Option<i32>` can stay one chain
    /// instead of breaking out of it to conditionally call [`Self::release_year`] — and the
    /// year *is* load-bearing for those fixtures, since `matcher::score` bonuses an exact
    /// match and a year-bounded browse filter excludes a series that has none.
    #[must_use]
    pub fn release_year_opt(mut self, year: Option<i32>) -> Self {
        self.release_year = year;
        self
    }

    /// Override the scan content hash. Only matters to a test about change detection, where two
    /// ingests of the same source must differ (or agree) in this value.
    #[must_use]
    pub fn content_hash(mut self, hash: Vec<u8>) -> Self {
        self.content_hash = hash;
        self
    }

    /// Ingest and return the full outcome, including the source id and the genuinely-new chapter
    /// numbers. Use this when the test is *about* ingest; [`Self::create`] when it only needs a
    /// series to exist.
    ///
    /// # Panics
    /// If the ingest fails, which in a test always means the fixture is wrong.
    pub async fn ingest(self) -> IngestOutcome {
        let normalized = normalize_title(self.title);
        let source_path = self
            .source_path
            .unwrap_or_else(|| format!("/s/{}", normalized.replace(' ', "-")));
        ingest_series(
            &self.db.pool,
            &ScannedSeries {
                provider_id: self.provider_id,
                source_path,
                provider_title: Some(self.title.to_owned()),
                meta: SeriesUpsert {
                    canonical_title: self.title.to_owned(),
                    normalized_title: normalized,
                    description: None,
                    cover_url: None,
                    content_type: self.content_type,
                    status: self.status,
                    release_year: self.release_year,
                },
                alt_titles: self.alt_titles,
                tags: self.tags,
                authors: self.authors,
                chapters: self
                    .chapters
                    .iter()
                    .map(|n| ChapterUpsert {
                        number: *n,
                        volume: None,
                        title: None,
                        path: format!("/c/{n}"),
                        published_at: None,
                    })
                    .collect(),
                content_hash: self.content_hash,
            },
            &MatchingConfig::default(),
        )
        .await
        .expect("seed series")
    }

    /// Ingest and return only the canonical series id.
    ///
    /// # Panics
    /// If the ingest fails, which in a test always means the fixture is wrong.
    pub async fn create(self) -> SeriesId {
        self.ingest().await.series_id
    }
}
