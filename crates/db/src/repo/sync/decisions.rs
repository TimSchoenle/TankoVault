//! The sync decision journal, and the blocklist an operator's "this match is wrong" writes to.
//!
//! Distinct from `sync_history`, which is the *reader's* log of what changed on their shelf. This
//! is the operator-facing record of what the engine considered and why, including the three
//! things history never carried: the series a remote entry failed to match, the fields both sides
//! already agreed on, and — the one that matters most — the title match that produced a mapping,
//! which used to be written with no score, no signals and no record of which title matched.

use serde_json::Value as Json;
use sqlx::PgExecutor;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use tankovault_domain::{SeriesId, UserId};

/// One decision to record. A reconciliation run produces many, so these are inserted as a batch.
#[derive(Debug, Clone)]
pub struct NewSyncDecision {
    pub user_id: UserId,
    /// `None` when a remote entry matched no local series — the case worth recording.
    pub series_id: Option<SeriesId>,
    pub provider: String,
    pub external_id: Option<String>,
    /// `match` | `progress` | `status` | `series` | `metadata`.
    pub scope: String,
    pub action: String,
    /// The stable slug for *why*.
    pub reason: String,
    pub policy: Option<String>,
    /// Whether anything was actually written. Most of a settled run is `false`, and separating
    /// the two is what lets the console show what changed without hiding what was considered.
    pub applied: bool,
    pub local_before: Option<String>,
    pub local_after: Option<String>,
    pub remote_before: Option<String>,
    pub remote_after: Option<String>,
    pub ancestor_local: Option<String>,
    pub ancestor_remote: Option<String>,
    pub match_score: Option<f32>,
    pub match_signals: Vec<String>,
    pub evidence: Json,
}

impl NewSyncDecision {
    /// A decision with only the identifying half filled in; the caller sets what applies.
    #[must_use]
    pub fn new(user_id: UserId, provider: &str, scope: &str, action: &str, reason: &str) -> Self {
        Self {
            user_id,
            series_id: None,
            provider: provider.to_owned(),
            external_id: None,
            scope: scope.to_owned(),
            action: action.to_owned(),
            reason: reason.to_owned(),
            policy: None,
            applied: false,
            local_before: None,
            local_after: None,
            remote_before: None,
            remote_after: None,
            ancestor_local: None,
            ancestor_remote: None,
            match_score: None,
            match_signals: Vec::new(),
            evidence: Json::Object(serde_json::Map::new()),
        }
    }
}

/// Record a run's decisions in one statement.
///
/// Set-based rather than one insert per decision: a reconciliation of a 900-entry library takes
/// a decision per entry per field, and a round trip each would cost more than the reconciliation.
///
/// # Errors
/// [`DbError::Sqlx`] only. An empty slice is a no-op, not an error.
pub async fn record_sync_decisions<'e, E: PgExecutor<'e>>(
    exec: E,
    run_id: Uuid,
    decisions: &[NewSyncDecision],
) -> DbResult<()> {
    if decisions.is_empty() {
        return Ok(());
    }
    let mut ids = Vec::with_capacity(decisions.len());
    let mut user_ids = Vec::with_capacity(decisions.len());
    let mut series_ids = Vec::with_capacity(decisions.len());
    let mut providers = Vec::with_capacity(decisions.len());
    let mut external_ids = Vec::with_capacity(decisions.len());
    let mut scopes = Vec::with_capacity(decisions.len());
    let mut actions = Vec::with_capacity(decisions.len());
    let mut reasons = Vec::with_capacity(decisions.len());
    let mut policies = Vec::with_capacity(decisions.len());
    let mut applied = Vec::with_capacity(decisions.len());
    let mut local_before = Vec::with_capacity(decisions.len());
    let mut local_after = Vec::with_capacity(decisions.len());
    let mut remote_before = Vec::with_capacity(decisions.len());
    let mut remote_after = Vec::with_capacity(decisions.len());
    let mut ancestor_local = Vec::with_capacity(decisions.len());
    let mut ancestor_remote = Vec::with_capacity(decisions.len());
    let mut match_scores = Vec::with_capacity(decisions.len());
    let mut signals = Vec::with_capacity(decisions.len());
    let mut evidence = Vec::with_capacity(decisions.len());

    for d in decisions {
        ids.push(Uuid::now_v7());
        user_ids.push(d.user_id.as_uuid());
        series_ids.push(d.series_id.map(SeriesId::as_uuid));
        providers.push(d.provider.clone());
        external_ids.push(d.external_id.clone());
        scopes.push(d.scope.clone());
        actions.push(d.action.clone());
        reasons.push(d.reason.clone());
        policies.push(d.policy.clone());
        applied.push(d.applied);
        local_before.push(d.local_before.clone());
        local_after.push(d.local_after.clone());
        remote_before.push(d.remote_before.clone());
        remote_after.push(d.remote_after.clone());
        ancestor_local.push(d.ancestor_local.clone());
        ancestor_remote.push(d.ancestor_remote.clone());
        match_scores.push(d.match_score);
        // A `text[][]` parameter would have to be rectangular, which per-decision signal lists
        // are not. Each list travels as JSON and is unpacked back into an array in the insert.
        signals.push(Json::Array(
            d.match_signals
                .iter()
                .map(|s| Json::String(s.clone()))
                .collect(),
        ));
        evidence.push(d.evidence.clone());
    }

    sqlx::query!(
        "INSERT INTO sync_decisions \
            (id, run_id, user_id, series_id, provider, external_id, scope, action, reason, \
             policy, applied, local_before, local_after, remote_before, remote_after, \
             ancestor_local, ancestor_remote, match_score, match_signals, evidence) \
         SELECT u.id, $1, u.user_id, u.series_id, u.provider, u.external_id, u.scope, u.action, \
                u.reason, u.policy, u.applied, u.local_before, u.local_after, u.remote_before, \
                u.remote_after, u.ancestor_local, u.ancestor_remote, u.match_score, \
                COALESCE(ARRAY(SELECT jsonb_array_elements_text(u.signals)), '{}'), u.evidence \
           FROM UNNEST($2::uuid[], $3::uuid[], $4::uuid[], $5::text[], $6::text[], $7::text[], \
                       $8::text[], $9::text[], $10::text[], $11::boolean[], $12::text[], \
                       $13::text[], $14::text[], $15::text[], $16::text[], $17::text[], \
                       $18::real[], $19::jsonb[], $20::jsonb[]) \
                AS u(id, user_id, series_id, provider, external_id, scope, action, reason, \
                     policy, applied, local_before, local_after, remote_before, remote_after, \
                     ancestor_local, ancestor_remote, match_score, signals, evidence)",
        run_id,
        &ids,
        &user_ids,
        &series_ids as &[Option<Uuid>],
        &providers,
        &external_ids as &[Option<String>],
        &scopes,
        &actions,
        &reasons,
        &policies as &[Option<String>],
        &applied,
        &local_before as &[Option<String>],
        &local_after as &[Option<String>],
        &remote_before as &[Option<String>],
        &remote_after as &[Option<String>],
        &ancestor_local as &[Option<String>],
        &ancestor_remote as &[Option<String>],
        &match_scores as &[Option<f32>],
        &signals,
        &evidence,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// One journal row as the console renders it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncDecisionRow {
    pub id: Uuid,
    pub run_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
    pub user_id: Uuid,
    pub username: Option<String>,
    pub series_id: Option<SeriesId>,
    pub series_title: Option<String>,
    pub provider: String,
    pub external_id: Option<String>,
    pub scope: String,
    pub action: String,
    pub reason: String,
    pub policy: Option<String>,
    pub applied: bool,
    pub local_before: Option<String>,
    pub local_after: Option<String>,
    pub remote_before: Option<String>,
    pub remote_after: Option<String>,
    pub ancestor_local: Option<String>,
    pub ancestor_remote: Option<String>,
    pub match_score: Option<f32>,
    pub match_signals: Vec<String>,
    pub evidence: Json,
    #[serde(with = "time::serde::rfc3339::option")]
    pub reverted_at: Option<OffsetDateTime>,
    pub reverted_by: Option<Uuid>,
    pub revert_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub flagged_at: Option<OffsetDateTime>,
    pub flagged_by: Option<Uuid>,
    pub flag_reason: Option<String>,
}

/// How the console narrows the journal.
#[derive(Debug, Default, Clone)]
pub struct SyncDecisionFilter {
    pub user_id: Option<UserId>,
    pub series_id: Option<SeriesId>,
    pub provider: Option<String>,
    pub action: Option<String>,
    pub run_id: Option<Uuid>,
    /// Only decisions that wrote something. The default view: a run is mostly considerations.
    pub applied_only: bool,
    pub flagged_only: bool,
}

/// The projection both reads share, so a column added to one cannot be forgotten by the other.
struct Row {
    id: Uuid,
    run_id: Uuid,
    decided_at: OffsetDateTime,
    user_id: Uuid,
    username: Option<String>,
    series_id: Option<Uuid>,
    series_title: Option<String>,
    provider: String,
    external_id: Option<String>,
    scope: String,
    action: String,
    reason: String,
    policy: Option<String>,
    applied: bool,
    local_before: Option<String>,
    local_after: Option<String>,
    remote_before: Option<String>,
    remote_after: Option<String>,
    ancestor_local: Option<String>,
    ancestor_remote: Option<String>,
    match_score: Option<f32>,
    match_signals: Vec<String>,
    evidence: Json,
    reverted_at: Option<OffsetDateTime>,
    reverted_by: Option<Uuid>,
    revert_reason: Option<String>,
    flagged_at: Option<OffsetDateTime>,
    flagged_by: Option<Uuid>,
    flag_reason: Option<String>,
}

impl From<Row> for SyncDecisionRow {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            run_id: r.run_id,
            decided_at: r.decided_at,
            user_id: r.user_id,
            username: r.username,
            series_id: r.series_id.map(SeriesId::from_uuid),
            series_title: r.series_title,
            provider: r.provider,
            external_id: r.external_id,
            scope: r.scope,
            action: r.action,
            reason: r.reason,
            policy: r.policy,
            applied: r.applied,
            local_before: r.local_before,
            local_after: r.local_after,
            remote_before: r.remote_before,
            remote_after: r.remote_after,
            ancestor_local: r.ancestor_local,
            ancestor_remote: r.ancestor_remote,
            match_score: r.match_score,
            match_signals: r.match_signals,
            evidence: r.evidence,
            reverted_at: r.reverted_at,
            reverted_by: r.reverted_by,
            revert_reason: r.revert_reason,
            flagged_at: r.flagged_at,
            flagged_by: r.flagged_by,
            flag_reason: r.flag_reason,
        }
    }
}

/// A page of the journal, newest first.
///
/// # Errors
/// [`DbError::Sqlx`] only; no match is an empty `Vec`.
pub async fn list_sync_decisions<'e, E: PgExecutor<'e>>(
    exec: E,
    filter: &SyncDecisionFilter,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<SyncDecisionRow>> {
    // Both joins are `LEFT`: an unmatched entry has no series by definition, and a decision must
    // stay readable after the user it belonged to is erased.
    let rows = sqlx::query_as!(
        Row,
        "SELECT d.id, d.run_id, d.decided_at, d.user_id, u.username, \
                d.series_id, s.canonical_title AS series_title, \
                d.provider, d.external_id, d.scope, d.action, d.reason, d.policy, d.applied, \
                d.local_before, d.local_after, d.remote_before, d.remote_after, \
                d.ancestor_local, d.ancestor_remote, d.match_score, d.match_signals, \
                d.evidence, d.reverted_at, d.reverted_by, d.revert_reason, \
                d.flagged_at, d.flagged_by, d.flag_reason \
           FROM sync_decisions d \
           LEFT JOIN series s ON s.id = d.series_id \
           LEFT JOIN users  u ON u.id = d.user_id \
          WHERE ($3::uuid IS NULL OR d.user_id = $3) \
            AND ($4::uuid IS NULL OR d.series_id = $4) \
            AND ($5::text IS NULL OR d.provider = $5) \
            AND ($6::text IS NULL OR d.action = $6) \
            AND ($7::uuid IS NULL OR d.run_id = $7) \
            AND (NOT $8::boolean OR d.applied) \
            AND (NOT $9::boolean OR d.flagged_at IS NOT NULL) \
          ORDER BY d.decided_at DESC \
          LIMIT $1 OFFSET $2",
        limit,
        offset,
        filter.user_id.map(UserId::as_uuid),
        filter.series_id.map(SeriesId::as_uuid),
        filter.provider.as_deref(),
        filter.action.as_deref(),
        filter.run_id,
        filter.applied_only,
        filter.flagged_only,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(SyncDecisionRow::from).collect())
}

/// One decision by id, for the service that has to undo it.
///
/// # Errors
/// [`DbError::NotFound`] when no such decision exists; otherwise [`DbError::Sqlx`].
pub async fn get_sync_decision<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
) -> DbResult<SyncDecisionRow> {
    // The `!` overrides are on `sync_decisions`' own columns, every one of them `NOT NULL`.
    // They are not decoration: with two `LEFT JOIN`s in the statement, sqlx's inference gives up
    // on the *base* table's nullability and calls all of it nullable, which types this row as
    // `Option` everywhere and does not compile. Asserting what the schema already guarantees is
    // the fix; a `?` here would push a schema fact nobody can violate out into every caller.
    let row = sqlx::query_as!(
        Row,
        "SELECT d.id AS \"id!\", d.run_id AS \"run_id!\", d.decided_at AS \"decided_at!\", \
                d.user_id AS \"user_id!\", u.username, \
                d.series_id, s.canonical_title AS series_title, \
                d.provider AS \"provider!\", d.external_id, d.scope AS \"scope!\", \
                d.action AS \"action!\", d.reason AS \"reason!\", d.policy, \
                d.applied AS \"applied!\", \
                d.local_before, d.local_after, d.remote_before, d.remote_after, \
                d.ancestor_local, d.ancestor_remote, d.match_score, \
                d.match_signals AS \"match_signals!\", \
                d.evidence AS \"evidence!\", d.reverted_at, d.reverted_by, d.revert_reason, \
                d.flagged_at, d.flagged_by, d.flag_reason \
           FROM sync_decisions d \
           LEFT JOIN series s ON s.id = d.series_id \
           LEFT JOIN users  u ON u.id = d.user_id \
          WHERE d.id = $1",
        id,
    )
    .fetch_optional(exec)
    .await?
    .ok_or(DbError::NotFound)?;
    Ok(row.into())
}

/// Stamp a decision reverted. Returns whether this call was the one that did it.
///
/// # Errors
/// [`DbError::Sqlx`] only; an unknown or already-reverted id is `Ok(false)`.
pub async fn mark_sync_decision_reverted<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    actor: Option<UserId>,
    reason: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE sync_decisions \
            SET reverted_at = now(), reverted_by = $2, revert_reason = $3 \
          WHERE id = $1 AND reverted_at IS NULL",
        id,
        actor.map(UserId::as_uuid),
        reason,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Stamp a decision wrong. Returns whether this call was the one that did it.
///
/// # Errors
/// [`DbError::Sqlx`] only; an unknown or already-flagged id is `Ok(false)`.
pub async fn flag_sync_decision<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    actor: Option<UserId>,
    reason: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE sync_decisions \
            SET flagged_at = now(), flagged_by = $2, flag_reason = $3 \
          WHERE id = $1 AND flagged_at IS NULL",
        id,
        actor.map(UserId::as_uuid),
        reason,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Refuse a title match permanently, and drop the mapping it produced.
///
/// Both halves are needed and neither is sufficient. Dropping the mapping alone leaves the next
/// reconciliation to run the same title match against the same catalogue and write the same row
/// back; recording the block alone leaves the wrong mapping in place, and the resolver never
/// re-derives a mapping it can look up.
///
/// # Errors
/// [`DbError::Sqlx`] only. Blocking an already-blocked pair is a no-op, so an operator clicking
/// twice does not see a failure.
pub async fn block_sync_match(
    pool: &sqlx::PgPool,
    provider: &str,
    external_id: &str,
    series_id: SeriesId,
    actor: Option<UserId>,
    reason: &str,
) -> DbResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "INSERT INTO sync_match_blocks (provider, external_id, series_id, reason, created_by) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (provider, external_id, series_id) DO UPDATE \
            SET reason = EXCLUDED.reason, created_by = EXCLUDED.created_by, created_at = now()",
        provider,
        external_id,
        series_id.as_uuid(),
        reason,
        actor.map(UserId::as_uuid),
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "DELETE FROM sync_mappings \
          WHERE series_id = $1 AND provider = $2 AND external_id = $3",
        series_id.as_uuid(),
        provider,
        external_id,
    )
    .execute(&mut *tx)
    .await?;

    // The cached remote snapshot points at the series too, and a stale one would re-seed the
    // mapping on the next run without the resolver being consulted at all.
    sqlx::query!(
        "UPDATE sync_remote_entries SET series_id = NULL \
          WHERE provider = $1 AND external_id = $2 AND series_id = $3",
        provider,
        external_id,
        series_id.as_uuid(),
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// The (external id, series) pairs this provider must never match again.
///
/// Read once per reconciliation rather than per entry: a blocklist is small and the alternative
/// is a query inside the resolver's per-entry loop.
///
/// # Errors
/// [`DbError::Sqlx`] only.
pub async fn blocked_sync_matches<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
) -> DbResult<Vec<(String, SeriesId)>> {
    let rows = sqlx::query!(
        "SELECT external_id, series_id FROM sync_match_blocks WHERE provider = $1",
        provider,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.external_id, SeriesId::from_uuid(r.series_id)))
        .collect())
}
