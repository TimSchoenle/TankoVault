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

pub use build::{
    BuildState, claim_repair_batch, delete_stale_generations, enqueue_repair, finish_build,
    list_stale_series, read_basis, read_build_state, repair_depth, start_build, update_build_stage,
    write_basis,
};
pub use embedding::{
    Neighbour, create_embedding_index, embedding_of, nearest_neighbours, write_embeddings,
};
pub use features::{
    FeatureRow, InternedFeature, SeriesFactsRow, count_feature_documents, dense_vocabulary,
    intern_features, list_series_facts, read_features, set_dense_indices, set_feature_stats,
    total_feature_documents, weighted_vectors, write_series_features,
};
pub use prior::{
    PriorInputs, page_series_ids, prior_inputs_for, summaries_in_order, top_by_prior, write_priors,
};
