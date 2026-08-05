-- Operator control of the recommender: tuning overrides, and the two grants that reach them
-- (docs/RECOMMENDATIONS.md §8, §10.1).

-- ---------------------------------------------------------------------------------------
-- Tuning overrides
-- ---------------------------------------------------------------------------------------
--
-- Same shape and same properties as `feature_flag_overrides`, deliberately: overrides only, so
-- an empty table is a fully working deployment, a new knob needs no seed row, and DELETEing a
-- row is the meaningful "revert to the shipped default" — distinct from writing that same
-- value, which records an operator decision that survives a future change of the default.
--
-- `key` is text because the registry is in code (`tankovault_domain::Tunable`). A row naming a
-- knob this build does not have is ignored and logged, never fatal, and stays visible on the
-- one page that can delete it.
--
-- `double precision`, one column, whatever the value means. The registry supplies the typing
-- and every reader clamps to the registry's range, so a hand-edited row cannot push a value
-- past a bound the API refuses — which is what makes the co-occurrence privacy floor
-- (§8.3, §12.2) hold against a direct database edit as well as against a `curl`.
CREATE TABLE tunable_overrides (
  key        text PRIMARY KEY,
  value      double precision NOT NULL,
  -- Why it was changed. Optional, but it is what the next operator needs and the audit record
  -- alone does not carry it to this page.
  note       text,
  updated_by uuid REFERENCES users(id) ON DELETE SET NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------------------
-- The grants that reach it
-- ---------------------------------------------------------------------------------------
--
-- Migration 0018 converted the departing `users.role` into explicit grants against two frozen
-- lists. Those lists reproduce the past and are never re-run, so a permission added later
-- reaches nobody unless it is granted here. Without this block `recsys.read`/`recsys.write`
-- would exist in code and be ungrantable on every existing deployment until someone with
-- `users.permissions` noticed the new checkbox.
--
-- Matching the presets as of this migration: `recsys.read` follows the operator bundle (a
-- diagnostic read), `recsys.write` follows the administrator bundle (it changes what every
-- reader on the deployment is shown). Existing grants identify each bundle — `flags.read`
-- without `flags.write` is the operator preset, `flags.write` is the administrator one.
INSERT INTO user_permissions (user_id, permission)
SELECT p.user_id, 'recsys.read'
FROM user_permissions p
WHERE p.permission = 'flags.read'
ON CONFLICT DO NOTHING;

INSERT INTO user_permissions (user_id, permission)
SELECT p.user_id, 'recsys.write'
FROM user_permissions p
WHERE p.permission = 'flags.write'
ON CONFLICT DO NOTHING;
