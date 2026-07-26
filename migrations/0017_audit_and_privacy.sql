-- Audit-log hardening (design §16) and the GDPR support columns.
--
-- The original `audit_log` recorded *what* happened but not *how it ended* or *where it
-- came from*, which is exactly what an audit trail is asked for after an incident. The
-- three columns below close that gap:
--
--   outcome    — a denied privileged action is the single most interesting audit record
--                there is, and previously it left no trace at all (handlers returned 403
--                before reaching the recording call).
--   actor_ip   — personal data under GDPR Art. 4(1); written only when the operator sets
--                `audit.record_ip`, hence nullable with no default.
--   user_agent — likewise gated behind `audit.record_user_agent`.
--
-- `actor_id` keeps its `ON DELETE SET NULL`, which is what makes erasure (Art. 17)
-- compatible with retaining the trail: deleting a user pseudonymises their audit records
-- rather than destroying the operator's evidence of privileged actions.
ALTER TABLE audit_log
  ADD COLUMN outcome    text NOT NULL DEFAULT 'success',
  ADD COLUMN actor_ip   inet,
  ADD COLUMN user_agent text;

ALTER TABLE audit_log
  ADD CONSTRAINT audit_log_outcome_check
  CHECK (outcome IN ('success', 'failure', 'denied'));

-- The retention sweep deletes by age. Neither existing index (actor_id, created_at) nor
-- (action, created_at) can serve a bare `created_at < $1` range scan.
CREATE INDEX audit_log_created_idx ON audit_log (created_at);
