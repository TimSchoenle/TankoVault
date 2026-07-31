//! GDPR data-subject operations: portability (Art. 20) and erasure (Art. 17).
//!
//! Both are implemented as a single explicit statement rather than as a loop over repo
//! functions, and that is deliberate: a table added later must force a visible change
//! *here*, where the reviewer is asking "what personal data do we hold?". A generic
//! walker would quietly keep working while silently omitting the new table from an export.

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::PgExecutor;
use tankovault_domain::UserId;

/// Assemble everything the system holds about one user as a portable JSON document.
///
/// Built server-side with `json_build_object` so the whole subject access request is one
/// consistent snapshot from one round trip, rather than a dozen queries that could
/// interleave with concurrent writes and produce a self-inconsistent export.
///
/// Every collection is an empty array rather than `null` for a user with no activity, so
/// a consumer parses one stable shape.
///
/// ## Redaction
///
/// Credentials are removed column-by-column in the SQL below, where the exclusion sits
/// next to the table it applies to:
///
/// - `users.password_hash` — an Argon2id verifier; exporting it would hand an offline
///   cracking target to anyone who later obtains the file.
/// - `refresh_tokens.token_hash` — a live session credential.
/// - `external_accounts.access_token` / `.refresh_token` — encrypted third-party OAuth
///   credentials. The *fact* of the link and its metadata are the user's data and are
///   exported; the bearer credentials are not, because an export is a commonly-emailed
///   artefact and these grant access to an entirely different service.
///
/// The subject's own `audit_log` entries **are** included: they are records about the
/// user and so fall within an access request. They are **projected**, not dumped:
/// `created_at`, `action`, `outcome`, and `target` only when the target is the subject
/// themselves. `detail` is dropped entirely.
///
/// Dumping whole rows leaked third parties. When the exporting user is an operator, their own
/// audit rows describe actions taken *on other people* — `admin/users.rs` records
/// `{"username": …, "email": …}` of the edited account, and `admin/privacy.rs` records another
/// data subject's id. GDPR Art. 15(4) is explicit that an access request must not adversely
/// affect the rights of others, and this is a file people forward by email. The compliance
/// goal — showing the subject what was recorded about them — is fully met by the projection.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `user_id` is
/// **not** [`crate::DbError::NotFound`]: every collection is a `coalesce(…, '[]')` subquery
/// and `profile` is `null`, so the export succeeds with an empty document. A caller that
/// needs to reject an unknown subject must check existence itself.
pub async fn export_user_data<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<Json> {
    let export = sqlx::query_scalar!(
        "SELECT json_build_object( \
           'exported_at', now(), \
           'profile', (SELECT to_jsonb(u) - 'password_hash' FROM users u WHERE u.id = $1), \
           'sessions', (SELECT coalesce(json_agg(to_jsonb(r) - 'token_hash' ORDER BY r.created_at), '[]'::json) \
                          FROM refresh_tokens r WHERE r.user_id = $1), \
           'watchlist', (SELECT coalesce(json_agg(to_jsonb(w) ORDER BY w.added_at), '[]'::json) \
                           FROM watchlist_entries w WHERE w.user_id = $1), \
           'read_progress', (SELECT coalesce(json_agg(to_jsonb(p) ORDER BY p.updated_at), '[]'::json) \
                               FROM read_progress p WHERE p.user_id = $1), \
           'notifications', (SELECT coalesce(json_agg(to_jsonb(n) ORDER BY n.created_at), '[]'::json) \
                               FROM notifications n WHERE n.user_id = $1), \
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

/// Erase a user and everything owned by them (GDPR Art. 17).
///
/// Returns `false` when no such user existed, so a caller can distinguish "erased" from
/// "was already gone" without a prior existence check that would race.
///
/// ## What this deletes, and what it deliberately does not
///
/// Every user-owned table declares `REFERENCES users(id) ON DELETE CASCADE`, so one
/// statement removes the profile, sessions, watchlist, progress, notifications, linked
/// accounts (including their encrypted tokens), sync state and history.
///
/// `audit_log.actor_id` is `ON DELETE SET NULL` instead. That is intentional and is what
/// makes erasure compatible with keeping an audit trail: the records of *what privileged
/// actions occurred* survive in pseudonymised form, while the identity linking them to a
/// person is destroyed. Retaining an unlinkable record of an administrative action rests
/// on legitimate interest (Art. 6(1)(f)), and once the actor reference is gone the record
/// is no longer personal data.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. "No such user" is `Ok(false)`
/// rather than [`crate::DbError::NotFound`], which is the distinction the return type exists
/// to make.
pub async fn erase_user<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<bool> {
    let deleted = sqlx::query!("DELETE FROM users WHERE id = $1", user_id.as_uuid())
        .execute(exec)
        .await?
        .rows_affected();
    Ok(deleted > 0)
}
