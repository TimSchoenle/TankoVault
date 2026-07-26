-- Permissions replace roles; runtime feature flags; account lifecycle; the GDPR request queue.
--
-- Four related changes land together because they are one authorization/administration model
-- and splitting them would leave intermediate states that do not authorize correctly (a
-- `users.role` column with no grants, or grants that nothing reads).

-- ---------------------------------------------------------------------------------------
-- 1. Per-user permission grants — the replacement for `users.role`.
-- ---------------------------------------------------------------------------------------
--
-- One row per (user, capability). `permission` is text rather than a Postgres enum on
-- purpose: the authoritative list is `tankovault_domain::Permission`, and adding a capability
-- there must not require a migration. The application parses each token and *drops* anything
-- it does not recognise (see `PermissionSet::from_tokens`), so an unknown row can only ever
-- narrow access — which is the direction a rollback must fail in.
--
-- `granted_by` is `ON DELETE SET NULL`, matching `audit_log.actor_id`: erasing the
-- administrator who granted a permission must not cascade into revoking it, and the grant
-- record survives in pseudonymised form.
CREATE TABLE user_permissions (
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  permission text NOT NULL,
  granted_at timestamptz NOT NULL DEFAULT now(),
  granted_by uuid REFERENCES users(id) ON DELETE SET NULL,
  PRIMARY KEY (user_id, permission)
);

-- "How many accounts still hold `users.permissions`?" is asked before every revoke and every
-- erasure, to refuse the operation that would leave the deployment with no administrator. The
-- primary key leads with `user_id` and cannot serve that lookup.
CREATE INDEX user_permissions_permission_idx ON user_permissions (permission);

-- Backfill from the role being removed, so an existing deployment keeps working across the
-- upgrade with the same effective access.
--
-- The two lists are the `PermissionPreset::Operator` and `PermissionPreset::Administrator`
-- expansions as of this migration. They are frozen copies by necessity — a migration must
-- reproduce the past, not follow the code — so they will drift from the presets over time.
-- That is correct: this converts historical roles once and is never re-run.
INSERT INTO user_permissions (user_id, permission)
SELECT u.id, p.permission
FROM users u
CROSS JOIN (VALUES
  ('providers.read'), ('providers.write'), ('providers.state'), ('providers.test'),
  ('scans.read'), ('scans.run'),
  ('merge.read'), ('merge.write'),
  ('sync.admin.read'),
  ('system.stats'), ('audit.read'), ('flags.read')
) AS p(permission)
WHERE u.role = 'operator';

INSERT INTO user_permissions (user_id, permission)
SELECT u.id, p.permission
FROM users u
CROSS JOIN (VALUES
  ('providers.read'), ('providers.write'), ('providers.create'), ('providers.delete'),
  ('providers.state'), ('providers.test'),
  ('scans.read'), ('scans.run'),
  ('merge.read'), ('merge.write'),
  ('sync.admin.read'), ('sync.admin.write'),
  ('users.read'), ('users.write'), ('users.permissions'), ('users.delete'), ('users.sessions'),
  ('privacy.read'), ('privacy.write'), ('privacy.export'),
  ('system.stats'), ('audit.read'),
  ('flags.read'), ('flags.write')
) AS p(permission)
WHERE u.role = 'admin';

-- ---------------------------------------------------------------------------------------
-- 2. Account lifecycle: suspension and last-login.
-- ---------------------------------------------------------------------------------------
--
-- Suspension is not modelled as "holds no permissions". Revoking every grant would still
-- leave the account able to sign in and read its own watchlist, which is not what suspending
-- an account means. It is an identity-level state, checked before authorization runs.
CREATE TYPE account_status AS ENUM ('active', 'suspended');

ALTER TABLE users
  ADD COLUMN status            account_status NOT NULL DEFAULT 'active',
  ADD COLUMN suspended_at      timestamptz,
  ADD COLUMN suspension_reason text,
  -- Shown in the user directory so an administrator can tell a dormant account from an
  -- active one before acting on it. Written on each successful login.
  ADD COLUMN last_login_at     timestamptz;

-- The role is gone, not deprecated: leaving the column would let a future query authorize
-- against it and silently bypass the grant table.
ALTER TABLE users DROP COLUMN role;
DROP TYPE user_role;

-- ---------------------------------------------------------------------------------------
-- 3. Feature-flag overrides.
-- ---------------------------------------------------------------------------------------
--
-- Overrides only. The shipped default for each feature lives in
-- `tankovault_domain::Feature::default_enabled`, so an empty table is a fully working
-- deployment, a new feature needs no seed row, and DELETEing a row is the meaningful
-- "revert to the shipped default" operation — distinct from setting the same value
-- explicitly, which records an operator decision.
--
-- `feature_key` is text for the same reason `user_permissions.permission` is: the registry is
-- in code. An override naming a feature this build does not have is ignored (and logged),
-- never fatal.
CREATE TABLE feature_flag_overrides (
  feature_key text PRIMARY KEY,
  enabled     boolean NOT NULL,
  -- Why the switch was flipped. Optional, but the reason is the thing the next operator
  -- needs and the audit record alone does not carry it to this page.
  note        text,
  updated_at  timestamptz NOT NULL DEFAULT now(),
  updated_by  uuid REFERENCES users(id) ON DELETE SET NULL
);

-- ---------------------------------------------------------------------------------------
-- 4. Data-subject requests (GDPR Chapter III).
-- ---------------------------------------------------------------------------------------
--
-- The self-service export and erasure endpoints satisfy Art. 15/17/20 for the common case.
-- This queue exists for the rest of the obligation: requests that require a human
-- (rectification of data the user cannot edit, restriction, objection), requests filed while
-- self-service is switched off, and the Art. 12(3) duty to respond within one month — which
-- means a request has to be a *tracked object* with a due date, not an HTTP call that either
-- happened or did not.
CREATE TYPE gdpr_request_kind AS ENUM (
  'access',         -- Art. 15: what do you hold about me
  'portability',    -- Art. 20: give it to me in a machine-readable form
  'rectification',  -- Art. 16: correct it
  'erasure',        -- Art. 17: delete it
  'restriction',    -- Art. 18: stop processing it, but keep it
  'objection'       -- Art. 21: stop processing it on legitimate-interest grounds
);

CREATE TYPE gdpr_request_status AS ENUM (
  'pending',
  'in_progress',
  'completed',
  'rejected',
  'cancelled'       -- withdrawn by the subject before it was resolved
);

-- `user_id` is `ON DELETE SET NULL` and there is deliberately **no snapshot of the subject's
-- email or name**. That is what makes this table compatible with the erasure it records: while
-- a request is open its subject exists and is reachable by join, and once an erasure completes
-- the row degrades by itself to "an erasure request was filed on D1 and completed on D2 by
-- operator O" — an accountability record (Art. 5(2)) that is no longer personal data. Copying
-- the email into this table would have quietly re-created the identifier the erasure destroyed.
CREATE TABLE gdpr_requests (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id       uuid REFERENCES users(id) ON DELETE SET NULL,
  kind          gdpr_request_kind NOT NULL,
  status        gdpr_request_status NOT NULL DEFAULT 'pending',
  -- What the subject asked for, in their words. Required for rectification (which field, to
  -- what) and useful context on every other kind.
  detail        text,
  requested_at  timestamptz NOT NULL DEFAULT now(),
  -- Art. 12(3): one month from receipt. Stored rather than computed so an extension
  -- (Art. 12(3) allows two further months for complex requests) is a value an operator can
  -- set, not a constant buried in a query.
  due_at        timestamptz NOT NULL DEFAULT now() + interval '30 days',
  claimed_by    uuid REFERENCES users(id) ON DELETE SET NULL,
  claimed_at    timestamptz,
  resolved_by   uuid REFERENCES users(id) ON DELETE SET NULL,
  resolved_at   timestamptz,
  -- How it was resolved, or why it was refused. Art. 12(4) requires the subject to be told
  -- the reasons for a refusal, so a rejection without one is incomplete.
  resolution_note text
);

-- The operator queue is ordered by urgency: soonest due first among the open ones.
CREATE INDEX gdpr_requests_queue_idx ON gdpr_requests (status, due_at);
-- A subject listing their own requests.
CREATE INDEX gdpr_requests_user_idx ON gdpr_requests (user_id, requested_at DESC);
