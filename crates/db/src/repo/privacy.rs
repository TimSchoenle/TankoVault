//! GDPR data-subject operations: portability (Art. 20) and erasure (Art. 17).
//!
//! Each is one explicit statement, not a loop over repo functions — a table added later must
//! force a visible change here, or a generic walker would silently omit it from the export.

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::PgExecutor;
use tankovault_domain::UserId;

/// Assemble everything the system holds about one user as one portable JSON document (one
/// round trip, so it can't interleave with concurrent writes).
///
/// Credentials (password hash, session/OAuth tokens) are excluded — an export is a
/// commonly-emailed file. Passkeys carry their metadata only: the serialised credential is the
/// library's business, and `credential_id` is withheld because observing one is the first half
/// of the registration-collision takeover `0022_passkeys.up.sql` blocks with its `UNIQUE`. The
/// subject's own `audit_log` rows are projected, not dumped (no `detail`; `target` only when it
/// names the subject), so an operator's export can't leak another subject's identity
/// (Art. 15(4)).
///
/// # Errors
/// `Sqlx` only; an unknown `user_id` returns an empty document, not `NotFound`.
pub async fn export_user_data<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<Json> {
    let export = sqlx::query_scalar!(
        "SELECT json_build_object( \
           'exported_at', now(), \
           'profile', (SELECT to_jsonb(u) - 'password_hash' FROM users u WHERE u.id = $1), \
           'sessions', (SELECT coalesce(json_agg(to_jsonb(r) - 'token_hash' ORDER BY r.created_at), '[]'::json) \
                          FROM refresh_tokens r WHERE r.user_id = $1), \
           'passkeys', (SELECT coalesce(json_agg(to_jsonb(k) - 'credential' - 'credential_id' - 'user_id' \
                                                 ORDER BY k.created_at), '[]'::json) \
                          FROM user_passkeys k WHERE k.user_id = $1), \
           'watchlist', (SELECT coalesce(json_agg(to_jsonb(w) ORDER BY w.added_at), '[]'::json) \
                           FROM watchlist_entries w WHERE w.user_id = $1), \
           'read_progress', (SELECT coalesce(json_agg(to_jsonb(p) ORDER BY p.updated_at), '[]'::json) \
                               FROM read_progress p WHERE p.user_id = $1), \
           'notifications', (SELECT coalesce(json_agg(to_jsonb(n) ORDER BY n.created_at), '[]'::json) \
                               FROM notifications n WHERE n.user_id = $1), \
           /* The recommendation profile. Derived from the watchlist, but disclosed separately \
              because it is a *profile* in the GDPR sense: an inference about the subject, not a \
              copy of what they entered. Art. 15(1)(h) asks for the logic, and the least this can \
              do is show the values. */ \
           'recommendation_affinity', (SELECT coalesce(json_agg(to_jsonb(a) - 'user_id' \
                                                                ORDER BY a.affinity DESC), '[]'::json) \
                                         FROM user_series_affinity a WHERE a.user_id = $1), \
           'recommendation_profile', (SELECT coalesce(json_agg(to_jsonb(t) - 'user_id' - 'embedding'), \
                                                      '[]'::json) \
                                        FROM user_taste_profile t WHERE t.user_id = $1), \
           'recommendation_feedback', (SELECT coalesce(json_agg(to_jsonb(f) - 'user_id' \
                                                                ORDER BY f.created_at), '[]'::json) \
                                         FROM recommendation_feedback f WHERE f.user_id = $1), \
           /* What the subject was actually shown, as opposed to what could be inferred about \
              them. A regenerating cache, which argues for leaving it out of an export — but not \
              out of a subject access request, where which recommendations this system put in \
              front of them is precisely the question being asked. */ \
           'recommendation_shelf', (SELECT coalesce(json_agg(to_jsonb(r) - 'user_id' \
                                                             ORDER BY r.built_at), '[]'::json) \
                                      FROM user_recommendations r WHERE r.user_id = $1), \
           'linked_accounts', (SELECT coalesce(json_agg(to_jsonb(e) - 'access_token' - 'refresh_token' \
                                                        ORDER BY e.created_at), '[]'::json) \
                                 FROM external_accounts e WHERE e.user_id = $1), \
           'sync_remote_entries', (SELECT coalesce(json_agg(to_jsonb(s) ORDER BY s.fetched_at), '[]'::json) \
                                     FROM sync_remote_entries s WHERE s.user_id = $1), \
           'sync_overrides', (SELECT coalesce(json_agg(to_jsonb(o)), '[]'::json) \
                                FROM series_sync_overrides o WHERE o.user_id = $1), \
           'sync_conflicts', (SELECT coalesce(json_agg(to_jsonb(c) ORDER BY c.detected_at), '[]'::json) \
                                FROM sync_conflicts c WHERE c.user_id = $1), \
           'sync_history', (SELECT coalesce(json_agg(to_jsonb(h) ORDER BY h.created_at), '[]'::json) \
                              FROM sync_history h WHERE h.user_id = $1), \
           'sync_decisions', (SELECT coalesce(json_agg( \
                                       to_jsonb(d) - 'user_id' - 'reverted_by' - 'flagged_by' \
                                     ORDER BY d.decided_at), '[]'::json) \
                                FROM sync_decisions d WHERE d.user_id = $1), \
           'audit_entries', (SELECT coalesce(json_agg(json_build_object( \
                                      'created_at', a.created_at, \
                                      'action', a.action, \
                                      'outcome', a.outcome, \
                                      'target', CASE WHEN a.target = $1::text THEN a.target ELSE NULL END \
                                    ) ORDER BY a.created_at), '[]'::json) \
                               FROM audit_log a WHERE a.actor_id = $1), \
           'permissions', (SELECT coalesce(json_agg(to_jsonb(g) - 'user_id' ORDER BY g.granted_at), '[]'::json) \
                             FROM user_permissions g WHERE g.user_id = $1), \
           'privacy_requests', (SELECT coalesce(json_agg(to_jsonb(q) - 'user_id' ORDER BY q.requested_at), '[]'::json) \
                                  FROM gdpr_requests q WHERE q.user_id = $1) \
         ) AS \"export!\"",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(export)
}

/// Erase a user and everything owned by them (GDPR Art. 17). Returns `false` when no such
/// user existed, so callers can distinguish "erased" from "already gone".
///
/// `audit_log.actor_id` is `ON DELETE SET NULL`, not cascaded — keeps the audit trail
/// (pseudonymised, under legitimate interest, Art. 6(1)(f)) once the identity link is gone.
///
/// # Errors
/// `Sqlx` only; "no such user" is `Ok(false)`, not `NotFound`.
pub async fn erase_user<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<bool> {
    let deleted = sqlx::query!("DELETE FROM users WHERE id = $1", user_id.as_uuid())
        .execute(exec)
        .await?
        .rows_affected();
    Ok(deleted > 0)
}
