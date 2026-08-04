//! The sparse half: reading what a series is, interning the vocabulary, and storing the vectors.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ContentType, SeriesId, SeriesStatus};
use tankovault_recsys::{FeatureKey, FeatureKind, SeriesFacts};
use uuid::Uuid;

/// One series' facts, ready for [`tankovault_recsys::extract`].
pub struct SeriesFactsRow {
    pub series_id: SeriesId,
    pub facts: SeriesFacts,
}

/// A page of series to extract features from, in `id` order.
///
/// Keyset, not `OFFSET`: the builder walks the whole catalogue and an offset walk re-reads
/// everything it has already passed. `after` is exclusive; pass `None` to start.
///
/// `country` is left `None` — `countryOfOrigin` is fetched from `AniList` but not yet persisted
/// on `series`. The extractor already supports the axis, so filling it later is a column and a
/// line here, not a model change.
///
///
/// The cursor is a **sentinel, not a nullable parameter**. ``WHERE $1::uuid IS NULL OR s.id > $1``
/// reads naturally and cannot use the primary key: the planner has to keep a plan valid for both
/// branches, so it scans the whole table and applies the `LIMIT` afterwards — which
/// `repo_query_plans` measured at 1.8x the cost ceiling. Starting from the nil UUID makes the
/// predicate unconditionally `s.id > $1`, an index range scan. No real id can collide with it:
/// every series id is a `UUIDv7` and carries a timestamp.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an exhausted walk is an empty `Vec`, which is the caller's
/// termination signal and must not be conflated with a failure.
pub async fn list_series_facts<'e, E: PgExecutor<'e>>(
    exec: E,
    after: Option<SeriesId>,
    limit: i64,
) -> DbResult<Vec<SeriesFactsRow>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        content_type: ContentType,
        status: SeriesStatus,
        release_year: Option<i32>,
        chapter_count: i64,
        tag_slugs: Vec<String>,
        tag_weights: Vec<f32>,
        author_slugs: Vec<String>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT s.id, \
                s.content_type AS \"content_type: ContentType\", \
                s.status AS \"status: SeriesStatus\", \
                s.release_year, \
                COALESCE(ch.n, 0) AS \"chapter_count!\", \
                COALESCE(tg.slugs, '{}') AS \"tag_slugs!\", \
                COALESCE(tg.weights, '{}') AS \"tag_weights!\", \
                COALESCE(au.slugs, '{}') AS \"author_slugs!\" \
         FROM series s \
         LEFT JOIN LATERAL ( \
           SELECT count(DISTINCT floor(c.number)) AS n \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = s.id \
         ) ch ON true \
         LEFT JOIN LATERAL ( \
           SELECT array_agg(t.slug ORDER BY t.slug) AS slugs, \
                  array_agg(st.weight ORDER BY t.slug) AS weights \
           FROM series_tags st JOIN tags t ON t.id = st.tag_id \
           WHERE st.series_id = s.id \
         ) tg ON true \
         LEFT JOIN LATERAL ( \
           SELECT array_agg(a.slug ORDER BY a.slug) AS slugs \
           FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
           WHERE sa.series_id = s.id \
         ) au ON true \
         WHERE s.id > $1 \
         ORDER BY s.id \
         LIMIT $2",
        after.map_or_else(Uuid::nil, SeriesId::as_uuid),
        limit,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| SeriesFactsRow {
            series_id: SeriesId::from_uuid(r.id),
            facts: SeriesFacts {
                content_type: r.content_type,
                status: r.status,
                release_year: r.release_year,
                chapter_count: r.chapter_count,
                tags: r.tag_slugs.into_iter().zip(r.tag_weights).collect(),
                authors: r.author_slugs,
                country: None,
            },
        })
        .collect())
}

/// The row [`intern_features`] reads back.
#[derive(FromRow)]
struct InternedRow {
    id: i32,
    kind: String,
    value: String,
}

/// One stored sparse vector, as [`weighted_vectors`] reads it.
#[derive(FromRow)]
struct VectorRow {
    series_id: Uuid,
    feature_ids: Vec<i32>,
    weights: Vec<f32>,
}

/// A feature and the id it interned to.
pub struct InternedFeature {
    pub key: FeatureKey,
    pub id: i32,
}

/// Intern a batch of feature keys, returning every one with its id.
///
/// `DO UPDATE`, not `DO NOTHING`: a conflicting row must still come back through `RETURNING`, and
/// `DO NOTHING` suppresses it. The update is a no-op write of the value onto itself — the
/// standard upsert-and-return shape, and the reason it is spelled so oddly.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn intern_features<'e, E: PgExecutor<'e>>(
    exec: E,
    keys: &[FeatureKey],
) -> DbResult<Vec<InternedFeature>> {
    let kinds: Vec<String> = keys.iter().map(|k| k.kind.as_str().to_owned()).collect();
    let values: Vec<String> = keys.iter().map(|k| k.value.clone()).collect();

    let rows = sqlx::query_as!(
        InternedRow,
        "INSERT INTO rec_features (kind, value) \
         SELECT * FROM unnest($1::text[], $2::text[]) \
         ON CONFLICT (kind, value) DO UPDATE SET value = EXCLUDED.value \
         RETURNING id, kind, value",
        &kinds,
        &values,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(InternedFeature {
                key: FeatureKey::new(parse_kind(&r.kind)?, r.value),
                id: r.id,
            })
        })
        .collect())
}

/// The SQL token back to its kind. Unknown tokens are dropped rather than defaulted: a kind this
/// build does not know is a row from a newer schema, and guessing would file it on the wrong axis.
fn parse_kind(token: &str) -> Option<FeatureKind> {
    Some(match token {
        "tag" => FeatureKind::Tag,
        "author" => FeatureKind::Author,
        "content_type" => FeatureKind::ContentType,
        "country" => FeatureKind::Country,
        "status" => FeatureKind::Status,
        "decade" => FeatureKind::Decade,
        "length" => FeatureKind::Length,
        _ => return None,
    })
}

/// Store one batch of extracted vectors.
///
/// `feature_ids` must be ascending — every reader merges two vectors on that assumption, and an
/// unsorted vector silently scores near zero against everything.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn write_series_features<'e, E: PgExecutor<'e>>(
    exec: E,
    series_ids: &[SeriesId],
    feature_ids: &[Vec<i32>],
    weights: &[Vec<f32>],
    digests: &[Vec<u8>],
    generation: i32,
) -> DbResult<u64> {
    let ids: Vec<Uuid> = series_ids.iter().copied().map(SeriesId::as_uuid).collect();
    // `unnest` cannot spread a two-dimensional array into rows, so the per-series arrays travel
    // as JSON and are cast back on arrival. The alternative — a statement per series — is a
    // round trip per row over a million rows.
    let features_json = serde_json::to_value(feature_ids).unwrap_or_default();
    let weights_json = serde_json::to_value(weights).unwrap_or_default();

    let affected = sqlx::query!(
        "INSERT INTO series_features (series_id, feature_ids, weights, digest, generation) \
         SELECT u.id, \
                ARRAY(SELECT jsonb_array_elements_text(f.elem)::int), \
                ARRAY(SELECT jsonb_array_elements_text(w.elem)::real), \
                u.digest, $5 \
         FROM unnest($1::uuid[], $4::bytea[]) WITH ORDINALITY AS u(id, digest, ord) \
         JOIN LATERAL (SELECT ($2::jsonb -> (u.ord - 1)::int) AS elem) f ON true \
         JOIN LATERAL (SELECT ($3::jsonb -> (u.ord - 1)::int) AS elem) w ON true \
         ON CONFLICT (series_id) DO UPDATE \
            SET feature_ids = EXCLUDED.feature_ids, \
                weights     = EXCLUDED.weights, \
                digest      = EXCLUDED.digest, \
                generation  = EXCLUDED.generation, \
                built_at    = now()",
        &ids,
        features_json,
        weights_json,
        digests,
        generation,
    )
    .execute(exec)
    .await?
    .rows_affected();
    Ok(affected)
}

/// How many series carry each feature, for the idf pass.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn count_feature_documents<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<(i32, i64)>> {
    #[derive(FromRow)]
    struct Row {
        feature_id: Option<i32>,
        documents: Option<i64>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT unnest(feature_ids) AS feature_id, count(*) AS documents \
         FROM series_features GROUP BY 1",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.feature_id?, r.documents?)))
        .collect())
}

/// How many series have a feature vector at all — the `N` in idf.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn total_feature_documents<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<i64> {
    let total = sqlx::query_scalar!("SELECT count(*) AS \"n!\" FROM series_features")
        .fetch_one(exec)
        .await?;
    Ok(total)
}

/// Write the vocabulary statistics computed from [`count_feature_documents`].
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn set_feature_stats<'e, E: PgExecutor<'e>>(
    exec: E,
    ids: &[i32],
    doc_counts: &[i32],
    weights: &[f32],
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE rec_features f \
            SET doc_count = u.doc_count, idf = u.idf \
         FROM unnest($1::int[], $2::int[], $3::real[]) AS u(id, doc_count, idf) \
         WHERE f.id = u.id",
        ids,
        doc_counts,
        weights,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Assign the projection's input positions to the most frequent dense-eligible features.
///
/// Clears every existing index first, so the assignment is a function of the current catalogue
/// rather than an accumulation of past ones. Authors are excluded here rather than in the
/// caller because the exclusion is a property of the model, not of any one build.
///
/// The `dense_index` a feature holds is what pins the basis' column order, so this must run
/// before the projection is solved and must not run between solving it and applying it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn set_dense_indices<'e, E: PgExecutor<'e>>(exec: E, cap: i64) -> DbResult<i64> {
    let assigned = sqlx::query_scalar!(
        "WITH cleared AS (UPDATE rec_features SET dense_index = NULL WHERE dense_index IS NOT NULL), \
              ranked AS ( \
                SELECT id, row_number() OVER (ORDER BY doc_count DESC, id) - 1 AS position \
                FROM rec_features \
                WHERE kind <> 'author' AND doc_count > 0 \
              ), \
              capped AS (SELECT id, position FROM ranked WHERE position < $1), \
              applied AS ( \
                UPDATE rec_features f SET dense_index = c.position \
                FROM capped c WHERE f.id = c.id \
                RETURNING 1 \
              ) \
         SELECT count(*) AS \"n!\" FROM applied",
        cap,
    )
    .fetch_one(exec)
    .await?;
    Ok(assigned)
}

/// The dense vocabulary: every feature with an input position, and its idf.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn dense_vocabulary<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<(i32, i32, f32)>> {
    #[derive(FromRow)]
    struct Row {
        id: i32,
        dense_index: Option<i32>,
        idf: f32,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, dense_index, idf FROM rec_features \
         WHERE dense_index IS NOT NULL ORDER BY dense_index",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.id, r.dense_index?, r.idf)))
        .collect())
}

/// One page of stored sparse vectors, in `series_id` order.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn read_features<'e, E: PgExecutor<'e>>(
    exec: E,
    after: Option<SeriesId>,
    limit: i64,
) -> DbResult<Vec<(SeriesId, Vec<i32>, Vec<f32>)>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        feature_ids: Vec<i32>,
        weights: Vec<f32>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT series_id, feature_ids, weights FROM series_features \
         WHERE series_id > $1 \
         ORDER BY series_id LIMIT $2",
        after.map_or_else(Uuid::nil, SeriesId::as_uuid),
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (SeriesId::from_uuid(r.series_id), r.feature_ids, r.weights))
        .collect())
}

/// The `rec_features` row behind [`weighted_vectors`]'s second half.
#[derive(FromRow)]
struct FeatRow {
    id: i32,
    kind: String,
    value: String,
    idf: f32,
}

/// A feature as the explanation surface names it.
pub struct FeatureRow {
    pub id: i32,
    pub kind: String,
    pub value: String,
    pub idf: f32,
}

/// The idf-weighted, normalised vectors for a handful of series, plus the features they name.
///
/// This is the join the module header describes: `series_features` stores term weights, and the
/// request path needs the weighted vector. Bounded by the caller's id list, so it stays two
/// index lookups regardless of catalogue size.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an id with no vector is simply absent from the result.
pub async fn weighted_vectors<'e, E: PgExecutor<'e> + Copy>(
    exec: E,
    series_ids: &[SeriesId],
) -> DbResult<(Vec<(SeriesId, Vec<(i32, f32)>)>, Vec<FeatureRow>)> {
    let ids: Vec<Uuid> = series_ids.iter().copied().map(SeriesId::as_uuid).collect();

    let vectors = sqlx::query_as!(
        VectorRow,
        "SELECT series_id, feature_ids, weights FROM series_features WHERE series_id = ANY($1)",
        &ids,
    )
    .fetch_all(exec)
    .await?;

    let mut needed: Vec<i32> = vectors.iter().flat_map(|v| v.feature_ids.clone()).collect();
    needed.sort_unstable();
    needed.dedup();

    let features = sqlx::query_as!(
        FeatRow,
        "SELECT id, kind, value, idf FROM rec_features WHERE id = ANY($1)",
        &needed,
    )
    .fetch_all(exec)
    .await?;

    let idf_of: std::collections::HashMap<i32, f32> =
        features.iter().map(|f| (f.id, f.idf)).collect();

    let weighted = vectors
        .into_iter()
        .map(|v| {
            let mut pairs: Vec<(i32, f32)> = v
                .feature_ids
                .iter()
                .zip(&v.weights)
                .map(|(id, w)| (*id, w * idf_of.get(id).copied().unwrap_or(1.0)))
                .collect();
            let mut magnitudes: Vec<f32> = pairs.iter().map(|(_, w)| *w).collect();
            tankovault_recsys::normalise(&mut magnitudes);
            for (pair, scaled) in pairs.iter_mut().zip(magnitudes) {
                pair.1 = scaled;
            }
            (SeriesId::from_uuid(v.series_id), pairs)
        })
        .collect();

    let rows = features
        .into_iter()
        .map(|f| FeatureRow {
            id: f.id,
            kind: f.kind,
            value: f.value,
            idf: f.idf,
        })
        .collect();
    Ok((weighted, rows))
}

/// The row [`exact_feature_matches`] reads.
#[derive(FromRow)]
struct ExactRow {
    series_id: Uuid,
    shared: i64,
}

/// A series matched by exact feature overlap, and how many of the asked-for features it carries.
pub struct ExactMatch {
    pub series_id: SeriesId,
    pub shared: i64,
}

/// Series carrying any of a set of features, most overlap first.
///
/// **This is the path that recovers what the dense projection destroys.** Authors are excluded
/// from the embedding entirely — a rank-128 approximation cannot represent a feature with a
/// document frequency of three — so the single most reliable signal in the catalogue, "same
/// author", is invisible to the ANN index. Here it is exact.
///
/// Callers must pass *rare* features only. The array-overlap operator is index-backed
/// (`series_features_gin`), but a feature carried by a third of the catalogue matches a third of
/// the catalogue, and no index makes that cheap.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn exact_feature_matches<'e, E: PgExecutor<'e>>(
    exec: E,
    features: &[i32],
    exclude: &[SeriesId],
    include_adult: bool,
    limit: i64,
) -> DbResult<Vec<ExactMatch>> {
    let excluded: Vec<Uuid> = exclude.iter().copied().map(SeriesId::as_uuid).collect();

    let rows = sqlx::query_as!(
        ExactRow,
        "SELECT f.series_id, \
                cardinality(ARRAY(SELECT unnest(f.feature_ids) INTERSECT SELECT unnest($1::int[]))) \
                  ::int8 AS \"shared!\" \
         FROM series_features f \
         JOIN series_prior p ON p.series_id = f.series_id AND p.recommendable \
         JOIN series s ON s.id = f.series_id AND (NOT s.is_adult OR $3) \
         WHERE f.feature_ids && $1::int[] \
           AND NOT (f.series_id = ANY($2)) \
         ORDER BY \"shared!\" DESC, f.series_id \
         LIMIT $4",
        features,
        &excluded,
        include_adult,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ExactMatch {
            series_id: SeriesId::from_uuid(r.series_id),
            shared: r.shared,
        })
        .collect())
}

/// The rarest features in a vector, by idf, for the exact retrieval path.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn rarest_features<'e, E: PgExecutor<'e>>(
    exec: E,
    features: &[i32],
    limit: i64,
) -> DbResult<Vec<i32>> {
    let rows = sqlx::query_scalar!(
        "SELECT id FROM rec_features \
          WHERE id = ANY($1) AND kind IN ('tag', 'author') \
          ORDER BY doc_count ASC, id \
          LIMIT $2",
        features,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Name a set of features, for a surface that shows them to a reader.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn feature_names<'e, E: PgExecutor<'e>>(
    exec: E,
    features: &[i32],
) -> DbResult<Vec<FeatureRow>> {
    let rows = sqlx::query_as!(
        FeatRow,
        "SELECT id, kind, value, idf FROM rec_features WHERE id = ANY($1)",
        features,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|f| FeatureRow {
            id: f.id,
            kind: f.kind,
            value: f.value,
            idf: f.idf,
        })
        .collect())
}
