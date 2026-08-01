-- Reverse of `0022_passkeys.up.sql`.
--
-- Destructive, and unavoidably so: dropping `user_passkeys` deletes every registered
-- credential, and a passkey cannot be re-derived — the private half never left the
-- authenticator. Rolling this back means every user re-registers every key. It is safe only
-- in the sense that no *other* data depends on these tables; password sign-in is untouched
-- and remains available to every account, which is why the rollback does not lock anyone out.
--
-- `webauthn_ceremonies` holds nothing worth preserving: every row is an in-flight challenge
-- that expires within minutes.
DROP TABLE IF EXISTS webauthn_ceremonies;
DROP TABLE IF EXISTS user_passkeys;
