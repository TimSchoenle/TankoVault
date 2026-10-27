//! Operator-console response bodies for `/v1/admin/*`.
//!
//! Converted from `tankovault-db` rows in `services/api` rather than derived directly from them, so
//! a `SELECT` column rename is a compile error instead of a silent public-API change. Types pin
//! their `OpenAPI` component name with `#[schema(as = ...)]` where it differs from the Rust name,
//! since `crates/api-client` and the frontend are generated from those names.

mod merges;
mod privacy;
mod recsys;
mod scans;
mod stats;
mod sync;
mod users;

pub use merges::{
    KeyRebuildView, MergeCandidateView, MergeDecisionView, MergeFullSweepView, MergePolicyView,
    MergeRevertBatchItemView, MergeRevertBatchOutcomeView, MergeRevertBatchView, MergeRevertedView,
    MergeSweepView, UndoSegmentView,
};
pub use privacy::AdminPrivacyRequestView;
pub use recsys::{RecsysBuildMode, RecsysBuildView};
pub use scans::{
    CancelScansBody, ClearFailuresBody, FailedTaskView, FailureGroupView, FailuresClearedView,
    ProviderScanHealthView, RunActivityView, RunTelemetryView, ScanActivityView, ScanCancelledView,
    ScanRunDetailView, ScanRunPageView, ScanRunView, ScanSummaryView, ScanTaskDetailView,
    ScanTriggeredView, StageTotalView, TaskEventView,
};
pub use stats::{AuditPageView, AuditView, ProviderStatView, SystemStatsView};
pub use sync::{
    RemoteEntryView, SyncAccountView, SyncDecisionView, SyncMappingView, SyncRevertedView,
    UnmappedSeriesView,
};
pub use users::{GrantView, UserDetailView, UserDirectoryPage, UserDirectoryRow};
