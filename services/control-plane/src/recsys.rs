//! The recommendation model builder (`docs/RECOMMENDATIONS.md` §6).
//!
//! Runs here rather than in the worker because a build is a *singleton* over the whole
//! catalogue, and this is the service that already holds the leader lock and already runs one
//! standing catalogue-wide job (the duplicate sweep). Putting it behind the same lock reuses the
//! mutual exclusion instead of inventing a second one.
//!
//! Nothing in the pipeline is resident in proportion to the catalogue. Extraction and projection
//! stream in batches; the vocabulary pass aggregates in the database; the one structure that
//! scales with anything is the covariance matrix, and that scales with the *vocabulary*
//! (`cap * cap` f64, ~32 MB at the default) rather than with the number of series.

use std::collections::HashMap;

use tankovault_db::PgPool;
use tankovault_db::repo::recsys;
use tankovault_domain::SeriesId;
use tankovault_recsys::{Basis, GramAccumulator};

/// How much work one build may do, and how the index is shaped.
#[derive(Debug, Clone, Copy)]
pub struct BuildBudget {
    /// Series per streamed batch. The only knob on peak memory in the streaming stages.
    pub batch: i64,
    /// Ceiling on a single incremental run, so a catalogue that grew by a million overnight
    /// does not turn the hourly pass into an all-day one.
    pub incremental_max: i64,
    /// Features that may shape the dense space — the covariance matrix's side.
    pub dense_input_cap: i64,
    pub hnsw_m: i32,
    pub hnsw_ef_construction: i32,
}

/// What a build did.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuildReport {
    pub generation: i32,
    pub series_built: i64,
    pub vocabulary: i64,
    pub dense_dims: i64,
}

/// Run one build.
///
/// Returns `None` when another build already holds the claim — the correct response to which is
/// to do nothing, not to wait: the other build is doing this one's work.
///
/// # Errors
/// Database failures. Whatever a failed run had already written stays written and is coherent:
/// every stage is idempotent and writes under a generation, so the next run redoes it.
pub async fn build(
    pool: &PgPool,
    budget: BuildBudget,
    full: bool,
) -> anyhow::Result<Option<BuildReport>> {
    let Some(generation) = recsys::start_build(pool, full).await? else {
        tracing::debug!("recsys build already in progress; skipping");
        return Ok(None);
    };

    // The claim is released *only* here. A build that returned early on an error without
    // finishing would leave `stage` stuck and every later run declining to start — a recommender
    // that silently stops updating and reports nothing wrong.
    let outcome = run_stages(pool, budget, full, generation).await;
    match &outcome {
        Ok(report) => {
            recsys::finish_build(
                pool,
                i32::try_from(report.series_built).unwrap_or(i32::MAX),
                i32::try_from(report.vocabulary).unwrap_or(i32::MAX),
                i32::try_from(report.dense_dims).unwrap_or(i32::MAX),
                None,
            )
            .await?;
        }
        Err(error) => {
            recsys::finish_build(pool, 0, 0, 0, Some(&error.to_string())).await?;
        }
    }
    outcome.map(Some)
}

async fn run_stages(
    pool: &PgPool,
    budget: BuildBudget,
    full: bool,
    generation: i32,
) -> anyhow::Result<BuildReport> {
    let mut report = BuildReport {
        generation,
        ..BuildReport::default()
    };

    if full {
        recsys::update_build_stage(pool, "full:features", 0).await?;
        report.series_built = extract_all(pool, budget, generation).await?;

        recsys::update_build_stage(pool, "full:vocabulary", 0).await?;
        report.vocabulary = vocabulary_pass(pool, budget.dense_input_cap).await?;

        recsys::update_build_stage(pool, "full:basis", 0).await?;
        let vocabulary = dense_map(pool).await?;
        let basis = solve_basis(pool, budget, &vocabulary).await?;
        persist_basis(pool, &basis).await?;
        report.dense_dims = i64::try_from(basis.width()).unwrap_or(i64::MAX);

        recsys::update_build_stage(pool, "full:embedding", 0).await?;
        project_all(pool, budget, generation, &basis, &vocabulary).await?;

        recsys::update_build_stage(pool, "full:index", 0).await?;
        recsys::create_embedding_index(pool, budget.hnsw_m, budget.hnsw_ef_construction).await?;

        recsys::update_build_stage(pool, "full:priors", 0).await?;
        prior_pass(pool, budget, generation, None).await?;

        recsys::delete_stale_generations(pool, generation).await?;
    } else {
        // No basis means no full build has ever run, so there is no space to project into.
        // Refusing is correct: projecting with a basis solved from a partial catalogue would
        // produce vectors that are not comparable with the stored ones, and the index would keep
        // answering with neighbours that are silently meaningless.
        let Some(basis) = load_basis(pool).await? else {
            anyhow::bail!("no projection basis yet: run a full build before an incremental one");
        };
        let vocabulary = dense_map(pool).await?;
        report.dense_dims = i64::try_from(basis.width()).unwrap_or(i64::MAX);
        report.vocabulary = i64::try_from(vocabulary.len()).unwrap_or(i64::MAX);

        let touched = incremental_targets(pool, budget).await?;
        report.series_built = i64::try_from(touched.len()).unwrap_or(i64::MAX);
        if !touched.is_empty() {
            extract_and_project(pool, budget, generation, &touched, &basis, &vocabulary).await?;
            prior_pass(pool, budget, generation, Some(&touched)).await?;
        }
        // Cheap and idempotent: a deployment whose index build was interrupted gets it back on
        // the next incremental pass instead of waiting for the next full one.
        recsys::create_embedding_index(pool, budget.hnsw_m, budget.hnsw_ef_construction).await?;
    }

    Ok(report)
}

/// Extract features for the whole catalogue, one keyset page at a time.
async fn extract_all(pool: &PgPool, budget: BuildBudget, generation: i32) -> anyhow::Result<i64> {
    let mut cursor: Option<SeriesId> = None;
    let mut built = 0_i64;
    loop {
        let page = recsys::list_series_facts(pool, cursor, budget.batch).await?;
        let Some(last) = page.last() else { break };
        cursor = Some(last.series_id);

        let stored = extract_batch(pool, &page, generation).await?;
        built += i64::try_from(stored.len()).unwrap_or(0);
        if built % (budget.batch * 20) == 0 {
            recsys::update_build_stage(
                pool,
                "full:features",
                i32::try_from(built).unwrap_or(i32::MAX),
            )
            .await?;
        }
    }
    Ok(built)
}

/// Intern one page's features and store its vectors. Returns the vectors as written.
async fn extract_batch(
    pool: &PgPool,
    page: &[recsys::SeriesFactsRow],
    generation: i32,
) -> anyhow::Result<Vec<(SeriesId, Vec<i32>, Vec<f32>)>> {
    let extracted: Vec<(SeriesId, Vec<(tankovault_recsys::FeatureKey, f32)>)> = page
        .iter()
        .map(|row| (row.series_id, tankovault_recsys::extract(&row.facts)))
        .collect();

    // One intern call per page, not per series: the vocabulary converges fast, so after the
    // first few pages almost every key already exists and this is a single indexed upsert.
    let mut keys: Vec<tankovault_recsys::FeatureKey> = extracted
        .iter()
        .flat_map(|(_, features)| features.iter().map(|(key, _)| key.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    let interned = recsys::intern_features(pool, &keys).await?;
    let id_of: HashMap<tankovault_recsys::FeatureKey, i32> =
        interned.into_iter().map(|f| (f.key, f.id)).collect();

    let mut ids = Vec::with_capacity(extracted.len());
    let mut feature_ids = Vec::with_capacity(extracted.len());
    let mut weights = Vec::with_capacity(extracted.len());
    let mut digests = Vec::with_capacity(extracted.len());
    let mut written = Vec::with_capacity(extracted.len());

    for (series_id, features) in &extracted {
        // Sorted by *id*, because every reader merges two vectors on that assumption.
        let mut pairs: Vec<(i32, f32)> = features
            .iter()
            .filter_map(|(key, weight)| Some((*id_of.get(key)?, *weight)))
            .collect();
        pairs.sort_by_key(|(id, _)| *id);

        let digest = tankovault_recsys::digest(features);
        ids.push(*series_id);
        feature_ids.push(pairs.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        weights.push(pairs.iter().map(|(_, w)| *w).collect::<Vec<_>>());
        digests.push(digest);
        written.push((
            *series_id,
            pairs.iter().map(|(id, _)| *id).collect(),
            pairs.iter().map(|(_, w)| *w).collect(),
        ));
    }

    recsys::write_series_features(pool, &ids, &feature_ids, &weights, &digests, generation).await?;
    Ok(written)
}

/// Count the vocabulary, write idf, and assign the projection's input positions.
async fn vocabulary_pass(pool: &PgPool, cap: i64) -> anyhow::Result<i64> {
    let total = recsys::total_feature_documents(pool).await?;
    let counts = recsys::count_feature_documents(pool).await?;

    let ids: Vec<i32> = counts.iter().map(|(id, _)| *id).collect();
    let doc_counts: Vec<i32> = counts
        .iter()
        .map(|(_, n)| i32::try_from(*n).unwrap_or(i32::MAX))
        .collect();
    let weights: Vec<f32> = counts
        .iter()
        .map(|(_, n)| tankovault_recsys::idf(*n, total))
        .collect();
    recsys::set_feature_stats(pool, &ids, &doc_counts, &weights).await?;

    // Must run before the basis is solved: `dense_index` is what pins the basis' column order.
    recsys::set_dense_indices(pool, cap).await?;
    Ok(i64::try_from(ids.len()).unwrap_or(i64::MAX))
}

/// `feature_id -> (input position, idf)`.
type DenseMap = HashMap<i32, (usize, f32)>;

async fn dense_map(pool: &PgPool) -> anyhow::Result<DenseMap> {
    let vocabulary = recsys::dense_vocabulary(pool).await?;
    Ok(vocabulary
        .into_iter()
        .filter_map(|(id, index, idf)| Some((id, (usize::try_from(index).ok()?, idf))))
        .collect())
}

/// Turn a stored term-weight vector into the idf-weighted, normalised dense row the projection
/// consumes. Features with no input position — authors, and anything past the cap — are absent
/// by construction.
fn dense_row(feature_ids: &[i32], weights: &[f32], vocabulary: &DenseMap) -> Vec<(usize, f32)> {
    let mut row: Vec<(usize, f32)> = feature_ids
        .iter()
        .zip(weights)
        .filter_map(|(id, weight)| {
            let (index, idf) = vocabulary.get(id)?;
            Some((*index, weight * idf))
        })
        .collect();
    let mut only_weights: Vec<f32> = row.iter().map(|(_, w)| *w).collect();
    tankovault_recsys::normalise(&mut only_weights);
    for (entry, weight) in row.iter_mut().zip(only_weights) {
        entry.1 = weight;
    }
    row
}

/// Project a row and widen it to the width `series_embedding` is declared with.
///
/// The basis can be narrower than the column. Orthogonal iteration cannot produce more than
/// `dim` orthonormal directions, so a catalogue whose *vocabulary* is smaller than
/// [`tankovault_recsys::EMBEDDING_DIMS`] — a new deployment, a small self-hosted instance, or any
/// fixture — yields a basis of that smaller width, and `halfvec(128)` rejects a shorter vector
/// outright.
///
/// Padding with zeros is exact rather than a fudge: the missing directions carry no variance, so
/// a zero contributes nothing to any cosine. Truncation is the impossible branch (`k` is capped
/// at `EMBEDDING_DIMS` when the basis is solved) and is written out anyway so a future change to
/// that cap cannot silently write vectors the column will not take.
fn embed(basis: &Basis, row: &[(usize, f32)]) -> Vec<f32> {
    let mut vector = basis.project(row);
    vector.resize(tankovault_recsys::EMBEDDING_DIMS, 0.0);
    vector
}

/// Stream the catalogue into a covariance matrix and reduce it to a projection.
async fn solve_basis(
    pool: &PgPool,
    budget: BuildBudget,
    vocabulary: &DenseMap,
) -> anyhow::Result<Basis> {
    let dim = vocabulary
        .values()
        .map(|(index, _)| *index + 1)
        .max()
        .unwrap_or(1);
    let mut gram = GramAccumulator::new(dim);

    let mut cursor: Option<SeriesId> = None;
    loop {
        let page = recsys::read_features(pool, cursor, budget.batch).await?;
        let Some((last, _, _)) = page.last() else {
            break;
        };
        cursor = Some(*last);
        for (_, feature_ids, weights) in &page {
            gram.push(&dense_row(feature_ids, weights, vocabulary));
        }
    }

    // Solving is pure CPU over a matrix that is already in memory; `spawn_blocking` keeps it off
    // the runtime that also serves this service's HTTP surface and drives its scheduler loops.
    let dims = tankovault_recsys::EMBEDDING_DIMS;
    let iterations = tankovault_recsys::BASIS_ITERATIONS;
    let basis = tokio::task::spawn_blocking(move || gram.basis(dims, iterations, 0x7461_6E6B))
        .await
        .map_err(|e| anyhow::anyhow!("basis solver panicked: {e}"))?;
    Ok(basis)
}

async fn persist_basis(pool: &PgPool, basis: &Basis) -> anyhow::Result<()> {
    let bytes: Vec<u8> = basis
        .as_columns()
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    recsys::write_basis(
        pool,
        &bytes,
        i32::try_from(basis.dim()).unwrap_or(i32::MAX),
        i32::try_from(basis.width()).unwrap_or(i32::MAX),
    )
    .await?;
    Ok(())
}

async fn load_basis(pool: &PgPool) -> anyhow::Result<Option<Basis>> {
    let Some((bytes, input_dim, dims)) = recsys::read_basis(pool).await? else {
        return Ok(None);
    };
    let columns: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    let dim = usize::try_from(input_dim).unwrap_or(0);
    let width = usize::try_from(dims).unwrap_or(0);
    // A basis of the wrong shape is refused rather than reshaped: it would project into a space
    // the stored vectors do not live in, and every neighbour it produced would be nonsense.
    Ok(Basis::from_columns(dim, width, columns))
}

/// Project every stored vector and write the embeddings.
async fn project_all(
    pool: &PgPool,
    budget: BuildBudget,
    generation: i32,
    basis: &Basis,
    vocabulary: &DenseMap,
) -> anyhow::Result<()> {
    let mut cursor: Option<SeriesId> = None;
    loop {
        let page = recsys::read_features(pool, cursor, budget.batch).await?;
        let Some((last, _, _)) = page.last() else {
            break;
        };
        cursor = Some(*last);

        let ids: Vec<SeriesId> = page.iter().map(|(id, _, _)| *id).collect();
        let vectors: Vec<Vec<f32>> = page
            .iter()
            .map(|(_, feature_ids, weights)| {
                embed(basis, &dense_row(feature_ids, weights, vocabulary))
            })
            .collect();
        recsys::write_embeddings(pool, &ids, &vectors, generation).await?;
    }
    Ok(())
}

/// The incremental work list: the repair queue first, then anything the live generation has not
/// reached.
async fn incremental_targets(pool: &PgPool, budget: BuildBudget) -> anyhow::Result<Vec<SeriesId>> {
    let mut targets = recsys::claim_repair_batch(pool, budget.incremental_max).await?;
    let claimed = i64::try_from(targets.len()).unwrap_or(i64::MAX);
    let remaining = budget.incremental_max.saturating_sub(claimed);
    if remaining > 0 {
        let state = recsys::read_build_state(pool).await?;
        let outdated = recsys::list_stale_series(pool, state.generation, remaining).await?;
        targets.extend(outdated);
    }
    targets.sort_by_key(|id| id.as_uuid());
    targets.dedup();
    Ok(targets)
}

/// Re-extract and re-embed a named set, fusing the two passes.
///
/// The full path cannot do this — the covariance matrix has to see everything before a basis
/// exists — but the incremental path projects with a basis that is already fixed, so the vectors
/// never have to be read back.
async fn extract_and_project(
    pool: &PgPool,
    budget: BuildBudget,
    generation: i32,
    targets: &[SeriesId],
    basis: &Basis,
    vocabulary: &DenseMap,
) -> anyhow::Result<()> {
    for chunk in targets.chunks(usize::try_from(budget.batch).unwrap_or(256)) {
        let facts = read_facts_of(pool, chunk).await?;
        let written = extract_batch(pool, &facts, generation).await?;

        let ids: Vec<SeriesId> = written.iter().map(|(id, _, _)| *id).collect();
        let vectors: Vec<Vec<f32>> = written
            .iter()
            .map(|(_, feature_ids, weights)| {
                embed(basis, &dense_row(feature_ids, weights, vocabulary))
            })
            .collect();
        recsys::write_embeddings(pool, &ids, &vectors, generation).await?;
    }
    Ok(())
}

/// Facts for a named set, read through the same paged query the full build uses.
///
/// One page per contiguous run rather than a bespoke `= ANY` query: the ids are sorted, the page
/// size is the batch size, and reusing the query keeps a single definition of "what a series is"
/// — a second one would be the place the two paths quietly disagree.
async fn read_facts_of(
    pool: &PgPool,
    ids: &[SeriesId],
) -> anyhow::Result<Vec<recsys::SeriesFactsRow>> {
    let wanted: std::collections::HashSet<uuid::Uuid> =
        ids.iter().copied().map(SeriesId::as_uuid).collect();
    let mut out = Vec::with_capacity(ids.len());
    let Some(first) = ids.first() else {
        return Ok(out);
    };
    // `after` is exclusive, so start just below the lowest wanted id.
    let mut cursor = predecessor_of(*first);
    while out.len() < ids.len() {
        let page =
            recsys::list_series_facts(pool, cursor, i64::try_from(ids.len()).unwrap_or(i64::MAX))
                .await?;
        let Some(last) = page.last() else { break };
        cursor = Some(last.series_id);
        let before = out.len();
        out.extend(
            page.into_iter()
                .filter(|row| wanted.contains(&row.series_id.as_uuid())),
        );
        if out.len() == before {
            // A whole page with nothing wanted in it means the remaining targets were deleted
            // between being queued and being read — a merge, most likely. Nothing to repair.
            break;
        }
    }
    Ok(out)
}

/// The id immediately below `id`, so a keyset walk starting there includes `id` itself.
fn predecessor_of(id: SeriesId) -> Option<SeriesId> {
    let value = id.as_uuid().as_u128();
    value
        .checked_sub(1)
        .map(|previous| SeriesId::from_uuid(uuid::Uuid::from_u128(previous)))
}

/// Recompute appeal priors, for the whole catalogue or for a named subset.
async fn prior_pass(
    pool: &PgPool,
    budget: BuildBudget,
    generation: i32,
    subset: Option<&[SeriesId]>,
) -> anyhow::Result<()> {
    let wanted: Option<std::collections::HashSet<uuid::Uuid>> =
        subset.map(|ids| ids.iter().copied().map(SeriesId::as_uuid).collect());

    let mut cursor: Option<SeriesId> = None;
    loop {
        let ids = recsys::page_series_ids(pool, cursor, budget.batch).await?;
        let Some(last) = ids.last() else { break };
        cursor = Some(*last);

        let page = recsys::prior_inputs_for(pool, &ids).await?;
        let rows: Vec<&recsys::PriorInputs> = page
            .iter()
            .filter(|row| {
                wanted
                    .as_ref()
                    .is_none_or(|set| set.contains(&row.series_id.as_uuid()))
            })
            .collect();
        if rows.is_empty() {
            continue;
        }

        let ids: Vec<SeriesId> = rows.iter().map(|r| r.series_id).collect();
        let priors: Vec<f32> = rows.iter().map(|r| prior_of(r)).collect();
        let watchers: Vec<i32> = rows
            .iter()
            .map(|r| i32::try_from(r.watchers).unwrap_or(i32::MAX))
            .collect();
        let velocities: Vec<f32> = vec![0.0; rows.len()];
        let recommendable: Vec<bool> = rows.iter().map(|r| is_recommendable(r)).collect();

        recsys::write_priors(
            pool,
            &ids,
            &priors,
            &watchers,
            &velocities,
            &recommendable,
            generation,
        )
        .await?;
    }
    Ok(())
}

/// Whether a series may be recommended at all.
///
/// A series nothing links to and nothing describes cannot be recommended usefully and should not
/// occupy a slot. Adult titles are excluded here *and* at every read: this flag is the model's
/// opinion, the read-time filter is the reader's.
/// The metadata bar counts *descriptive* features — tags and authors — not all of them. Status,
/// decade and length come from columns every series has, so a bar counting those would admit a
/// completely unenriched series on the strength of three facts that distinguish it from nothing.
/// Two is the minimum that can place a series relative to another.
fn is_recommendable(inputs: &recsys::PriorInputs) -> bool {
    inputs.has_active_source
        && inputs.chapters > 0
        && inputs.descriptive_features >= 2
        && !inputs.is_adult
}

/// Blend the appeal signals into `[0, 1]`.
///
/// The watcher term is confidence-scaled by the catalogue's own scale rather than trusted
/// outright: on a new deployment `watchers` is a handful of arbitrary early watchlists, and
/// letting it dominate would rank the catalogue by the first three people to sign up. The
/// remaining terms need no users at all.
fn prior_of(inputs: &recsys::PriorInputs) -> f32 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "counts are small and feed a saturating curve, not an exact computation"
    )]
    let saturate = |value: i64, knee: f32| -> f32 {
        let value = value.max(0) as f32;
        value / (value + knee)
    };
    let score = inputs
        .external_score
        .map_or(0.0, |value| (value / 100.0).clamp(0.0, 1.0));
    let popularity = inputs
        .external_popularity
        .map_or(0.0, |value| saturate(i64::from(value), 20_000.0));

    let blended = 0.30 * saturate(inputs.watchers, 25.0)
        + 0.30 * score
        + 0.25 * popularity
        + 0.15 * saturate(inputs.sources, 3.0);
    blended.clamp(0.0, 1.0)
}
