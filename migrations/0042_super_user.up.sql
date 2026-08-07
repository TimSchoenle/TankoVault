-- The super user: one grant that answers every permission check, held by the account this
-- deployment was installed with and by nobody else.
--
-- `system.superuser` is an ordinary `user_permissions` row on purpose — it resolves, audits and
-- displays through the paths every other grant already uses. What makes it different is in the
-- code (`PermissionSet::has` answers true to everything for it, and the permission editor omits
-- it from the catalogue it offers); what makes it unforgeable is the index below.

-- At most one super user per deployment, whoever writes the row and by whatever route. A unique
-- index on a constant expression is how "one row matching this predicate, in the whole table"
-- is stated. The bootstrap seed depends on it: re-running an install against a populated
-- database conflicts here instead of minting a second owner.
CREATE UNIQUE INDEX user_permissions_single_super_user
  ON user_permissions ((true)) WHERE permission = 'system.superuser';

-- Retroactive grant, so an installation that predates this migration ends up where a fresh one
-- starts: its owner already exists, created by the same seed step, and re-running that step
-- would change nothing.
--
-- The *earliest account that still administers permissions*, not simply the earliest account.
-- `users.permissions` is already documented as equivalent to full control — its holder can
-- grant themselves anything — so this promotes nobody who could not already promote themselves,
-- whereas keying on creation time alone would hand the deployment to whoever happened to
-- register first on a server that was bootstrapped later.
INSERT INTO user_permissions (user_id, permission)
SELECT u.id, 'system.superuser'
FROM users u
WHERE EXISTS (
        SELECT 1 FROM user_permissions p
        WHERE p.user_id = u.id AND p.permission = 'users.permissions'
      )
ORDER BY u.created_at, u.id
LIMIT 1;
