-- Reverse of `0043_multi_factor_auth.up.sql`.
--
-- Destructive in one direction that matters: dropping `user_totp` and `user_recovery_codes`
-- un-enrols every second factor, and neither can be re-derived — a TOTP secret exists only
-- here and in the authenticator app, and a recovery code was shown once. Rolling back does
-- not lock anyone out (password sign-in is untouched and no longer asks for a second leg),
-- but every enrolled user has to enrol again from scratch.
--
-- Security keys are handled differently, and this is the part to read before editing. The
-- `purpose` column cannot simply be dropped: the rows it marks `'security_key'` would silently
-- become passkeys — first-factor, discoverable sign-in credentials — in a table the pre-0043
-- code reads without any purpose filter. A hardware key registered as a second factor would
-- become a way to sign in on its own, which is a privilege escalation performed by a rollback.
-- So they are deleted first, then the column goes.
DELETE FROM user_webauthn_credentials WHERE purpose = 'security_key';

DROP TABLE IF EXISTS step_up_grants;
DROP TABLE IF EXISTS mfa_challenges;
DROP TABLE IF EXISTS user_recovery_codes;
DROP TABLE IF EXISTS user_totp;

-- In-flight ceremonies of the new kinds are dropped before the CHECK narrows again, or the
-- constraint would refuse to validate against rows already in the table.
DELETE FROM webauthn_ceremonies
  WHERE kind IN ('register_security_key', 'authenticate_security_key');
ALTER TABLE webauthn_ceremonies DROP CONSTRAINT webauthn_ceremony_kind;
ALTER TABLE webauthn_ceremonies ADD CONSTRAINT webauthn_ceremony_kind
  CHECK (kind IN ('register', 'authenticate'));

ALTER TABLE user_webauthn_credentials
  DROP CONSTRAINT user_webauthn_credential_purpose;
ALTER TABLE user_webauthn_credentials DROP COLUMN purpose;

DROP INDEX user_webauthn_credentials_user_idx;
ALTER TABLE user_webauthn_credentials RENAME TO user_passkeys;
CREATE INDEX user_passkeys_user_idx ON user_passkeys (user_id, created_at DESC);
