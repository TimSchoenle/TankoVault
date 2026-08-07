-- Rolling back leaves every account with the permissions it was explicitly granted; only the
-- implicit "everything" goes away.
DELETE FROM user_permissions WHERE permission = 'system.superuser';
DROP INDEX IF EXISTS user_permissions_single_super_user;
