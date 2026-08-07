//! Persistence for the recommendation model (`docs/RECOMMENDATIONS.md` §5.2, §6).
//!
//! Everything here is *derived*: drop it all and a rebuild reproduces it from `series` and its
//! link tables. That is what makes `generation` sufficient for an atomic-ish swap, and why none
//! of it appears in the GDPR export — it holds no personal data.
//!
//! # Where the idf lives
//!
//! `series_features.weights` holds **term weights**, not the final TF-IDF vector. Extraction runs
//! before the vocabulary has been counted, so idf is not known yet; a second pass to fold it in
//! would rewrite every row in the catalogue for a number that is one small join away. Callers
//! that need the weighted vector — the projection, and scoring a candidate pair — multiply by
//! `rec_features.idf` themselves and normalise. [`features::weighted_vectors`] is that join for
//! the request path; the builder does it in memory while streaming.

mod build;
mod embedding;
mod features;
mod prior;
mod reader;

pub use build::{
    BuildClaim, BuildState, ModelCoverage, claim_repair_batch, delete_stale_generations,
    enqueue_repair, finish_build, list_stale_series, read_basis, read_build_state,
    read_model_coverage, repair_depth, start_build, touch_build, update_build_stage, write_basis,
};
pub use embedding::{
    Neighbour, create_embedding_index, embedding_of, mean_embedding, nearest_excluding,
    nearest_neighbours, set_ef_search, write_embeddings,
};
pub use features::{
    ExactMatch, FeatureRow, InternedFeature, SeriesFactsRow, count_feature_documents,
    dense_vocabulary, exact_feature_matches, feature_names, intern_features, list_series_facts,
    rarest_features, read_features, set_dense_indices, set_feature_stats, total_feature_documents,
    weighted_vectors, write_series_features,
};
pub use prior::{
    PriorInputs, page_series_ids, prior_inputs_for, summaries_in_order, top_by_prior, write_priors,
};
pub use reader::{
    ReaderInteraction, TasteProfile, clear_shelf, mark_profile_stale,
    mark_profiles_stale_for_series, read_profile, read_shelf, reader_interactions, record_feedback,
    replace_affinity, suppressed_series, top_affinity, write_profile, write_shelf,
};
