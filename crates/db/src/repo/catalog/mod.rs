//! Catalog write + read path: canonical series, provider sources, and chapters.
//! Every write is idempotent (`ON CONFLICT`) so at-least-once task delivery is safe.

pub mod browse;
pub mod chapters;
pub mod enrichment;
pub mod ingest;
pub mod series;
pub mod sources;

pub use browse::*;
pub use chapters::*;
pub use enrichment::*;
pub use ingest::*;
pub use series::*;
pub use sources::*;
