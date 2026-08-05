-- Reverting drops the operator's tuning: the compiled defaults are what a build without this
-- table runs on, so there is nothing to preserve the values for.
DROP TABLE IF EXISTS tunable_overrides;

DELETE FROM user_permissions WHERE permission IN ('recsys.read', 'recsys.write');
