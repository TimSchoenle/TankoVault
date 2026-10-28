//! Persistence for the recommendation model (`docs/RECOMMENDATIONS.md` §5.2, §6).
//!
//! The catalogue model (features, embeddings, priors, build state) is *derived*: a rebuild
//! reproduces it from `series` and its link tables, and it holds no personal data. The reader
//! records (affinity, taste profiles, feedback, shelves) are per user and are part of the GDPR
//! export, through [`crate::repo::privacy::export_user_data`].
//!
//! `series_features.weights` holds **term weights**, not the final TF-IDF vector: extraction runs
//! before the vocabulary is counted, and folding idf in would rewrite every row for a number one
//! join away. The projection and pair scoring multiply by `rec_features.idf` and normalise;
//! [`features::weighted_vectors`] is that join for the request path, and the builder does it in
//! memory.

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
