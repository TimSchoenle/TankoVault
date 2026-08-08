-- Multi-factor authentication: TOTP, WebAuthn security keys, recovery codes, and the two
-- short-lived grants the flows need — a pending sign-in and a step-up ("sudo") elevation.
--
-- ---------------------------------------------------------------------------------------
-- Why the passkey table becomes a credential table instead of gaining a sibling
-- ---------------------------------------------------------------------------------------
-- A security key is the same artefact as a passkey — a WebAuthn credential — used at a
-- different point in the flow. The obvious schema is a second table shaped like the first, and
-- it is wrong for one reason: `credential_id` would then be unique *per table*, so the same
-- authenticator could hold a row in both. That user's second factor would be satisfied by the
-- very device that satisfied their first, and one touch would clear both legs. Two-factor
-- authentication would be off for them, with nothing in the schema, the API or the account page
-- saying so.
--
-- `excludeCredentials` at ceremony start already refuses the second registration in the happy
-- path, and both registration handlers pass the union of the user's credentials. But that is a
-- check-then-act across two HTTP requests; the constraint that makes the race impossible is
-- `UNIQUE (credential_id)` covering both purposes, and that exists only if the rows share a
-- table. So they share a table, and `purpose` says which leg each row serves.
--
-- The rename is the whole cost: `user_passkeys` holding security keys would be a name that
-- lies, and this schema has to be readable by whoever debugs a failed sign-in at 3am.
ALTER TABLE user_passkeys RENAME TO user_webauthn_credentials;
ALTER INDEX user_passkeys_user_idx RENAME TO user_webauthn_credentials_user_idx;

-- 'passkey'    — a discoverable, first-factor sign-in credential. What every existing row is.
-- 'security_key' — a second factor, presented only after a password has already verified.
--
-- The default exists for the backfill and is dropped immediately: a new row must state which
-- leg it serves, because the two are enforced differently and a silent default would pick the
-- weaker reading for whoever forgets.
ALTER TABLE user_webauthn_credentials
  ADD COLUMN purpose text NOT NULL DEFAULT 'passkey';
ALTER TABLE user_webauthn_credentials
  ALTER COLUMN purpose DROP DEFAULT;
ALTER TABLE user_webauthn_credentials
  ADD CONSTRAINT user_webauthn_credential_purpose
  CHECK (purpose IN ('passkey', 'security_key'));

-- The list views are per (user, purpose): the account page shows passkeys and security keys as
-- separate cards, and a second-factor challenge must never offer a passkey.
DROP INDEX user_webauthn_credentials_user_idx;
CREATE INDEX user_webauthn_credentials_user_idx
  ON user_webauthn_credentials (user_id, purpose, created_at DESC);

-- Two more ceremony kinds, for the same reason `0022` introduced the column: a `finish` leg
-- must refuse state produced by a different flow. Registering a security key and asserting one
-- are distinct from their passkey counterparts — a passkey registration state accepted by the
-- security-key finish handler would install a first-factor credential in the second-factor
-- list, where nothing checks it is discoverable.
ALTER TABLE webauthn_ceremonies DROP CONSTRAINT webauthn_ceremony_kind;
ALTER TABLE webauthn_ceremonies ADD CONSTRAINT webauthn_ceremony_kind
  CHECK (kind IN ('register', 'authenticate', 'register_security_key', 'authenticate_security_key'));

-- ---------------------------------------------------------------------------------------
-- TOTP
-- ---------------------------------------------------------------------------------------
-- One enrolment per account. A second authenticator app is not a second factor — it is the
-- same shared secret in two places — so the useful plural is security keys, and this table is
-- keyed by the user.
--
-- `secret` is **ciphertext**, sealed by `tankovault_auth::Sealer` (AES-256-GCM,
-- `nonce || ciphertext-with-tag`) under `auth.mfa_encryption_key`. Unlike a password hash, a
-- TOTP secret is symmetric: whoever reads the column can mint codes. A database dump must
-- therefore not be enough, which is what the separate key buys — it lives in the process
-- environment, not in Postgres. `bytea` rather than `text` because that is what the sealer
-- returns; base64ing it would add a decode step and an encoding convention to get wrong.
CREATE TABLE user_totp (
  user_id      uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  secret       bytea NOT NULL,
  -- What the authenticator app shows this account as, echoed into the provisioning URI.
  label        text NOT NULL,
  -- NULL while enrolment is half-done: the secret is issued and shown, but the user has not
  -- yet proved they scanned it. An unconfirmed row must never satisfy a sign-in challenge or
  -- count as an enrolled factor, or a user who closed the tab mid-enrolment is locked out of
  -- their own account by a secret they never stored.
  confirmed_at timestamptz,
  -- The last RFC 6238 time step this secret has been accepted at. A TOTP code is valid for its
  -- whole 30-second window and the implementation accepts one step of clock skew either side,
  -- so without this a code observed in transit — a shoulder-surfed screen, a proxy log — is
  -- replayable for up to 90 seconds. Verification refuses any step at or below this value.
  last_step    bigint,
  created_at   timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------------------
-- Recovery codes
-- ---------------------------------------------------------------------------------------
-- The escape hatch, and the reason enrolling a second factor is not a way to lose an account.
-- Issued as a set when the first factor is confirmed, shown exactly once, and consumed one at
-- a time.
--
-- `code_hash` is a SHA-256 hex digest, not an argon2 PHC string, and that is deliberate:
-- these are server-generated high-entropy tokens, not user-chosen secrets, so there is no
-- dictionary to slow down. Argon2 would instead mean one deliberately expensive hash per
-- candidate row on every verification — ten of them for a ten-code set — which is a denial of
-- service the attacker gets to trigger. `refresh_tokens.token_hash` is hashed the same way for
-- the same reason.
--
-- Rows are kept after use rather than deleted so the account page can say "3 of 10 remaining"
-- and so the audit trail has something to point at. `UNIQUE` is global: a collision across
-- users would let one account's code open another's.
CREATE TABLE user_recovery_codes (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  code_hash  text NOT NULL UNIQUE,
  used_at    timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);

-- The only read that matters: how many codes this account has left, and which unused row a
-- presented code matches. Partial, because a used code is never a candidate again.
CREATE INDEX user_recovery_codes_unused_idx
  ON user_recovery_codes (user_id) WHERE used_at IS NULL;

-- ---------------------------------------------------------------------------------------
-- Pending sign-in
-- ---------------------------------------------------------------------------------------
-- Between "the password verified" and "a second factor was presented" there is a state that
-- must be carried across two requests, and it must not be a session — the whole point is that
-- the caller is not authenticated yet. So `POST /v1/auth/login` answers with a handle to this
-- row instead of a token, and `POST /v1/auth/mfa/verify` trades the handle plus a factor for
-- the session it withheld.
--
-- In Postgres, not Redis, for the reason `0022_passkeys.up.sql` gives at length: Redis is
-- optional in this deployment, and behind two api replicas an optional store would fail the
-- second leg roughly half the time.
--
-- `token_hash`, not the id: the handle is a bearer credential for the rest of the sign-in, so
-- what is stored is a digest and what the client holds is the only copy. A row read out of a
-- backup, a slow-query log or a `pg_stat_activity` snapshot is then not enough to complete
-- somebody's sign-in.
CREATE TABLE mfa_challenges (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash text NOT NULL UNIQUE,
  -- A TOTP code is six digits. Without a cap, a challenge that lives five minutes is a million
  -- guesses against a one-in-a-million secret, which is not a second factor. The handler
  -- deletes the row once this passes its ceiling, so the cost of exhausting it is a fresh
  -- password sign-in — itself rate-limited on the auth budget.
  attempts   integer NOT NULL DEFAULT 0,
  expires_at timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

-- Drives the expiry sweep, alongside `webauthn_ceremonies_expiry_idx`. Abandoned rows are the
-- common case — a user who reaches the code prompt and gives up — so something has to delete
-- them or this table grows without bound.
CREATE INDEX mfa_challenges_expiry_idx ON mfa_challenges (expires_at);

-- ---------------------------------------------------------------------------------------
-- Step-up grants
-- ---------------------------------------------------------------------------------------
-- Proof that a second factor was presented *recently*, by an already-authenticated caller,
-- which is what sensitive routes require instead of the password re-entry they used to take.
-- A password re-entry defended nothing: whoever stole the password to sign in could type it
-- again to change the email, enrol their own passkey and make the takeover permanent.
--
-- Server-side, and resolved per request, because `AccessClaims` deliberately carries no
-- authorization state — a claim is fixed at minting, and an elevation has to be revocable
-- before it expires. It is keyed by a hashed opaque token the client presents in `X-Step-Up`
-- rather than by the refresh family, because the desktop build holds no cookie jar and would
-- have no family to key on.
--
-- Deliberately **not** scoped to a single action. The grant's job is to prove the second factor
-- is present, which defends a leaked access token; binding each grant to one endpoint would
-- cost a prompt per click for no gain, since anything able to forge one request from the SPA
-- can forge the step-up request beside it. `method` records which factor was presented so the
-- audit trail can distinguish a hardware-key elevation from a recovery-code one.
CREATE TABLE step_up_grants (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash text NOT NULL UNIQUE,
  -- 'totp' | 'security_key' | 'recovery_code' | 'password'. Text rather than an enum type, as
  -- with `webauthn_ceremonies.kind`: a private detail of one module, checked here so a typo in
  -- a query cannot write an unknown method. 'password' is the fallback for an account with no
  -- factor enrolled at all, and is refused the moment one exists.
  method     text NOT NULL,
  expires_at timestamptz NOT NULL,
  -- Set when the grant is invalidated ahead of its expiry: a password change, a sign-out, or
  -- the removal of the factor that produced it.
  revoked_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT step_up_grant_method
    CHECK (method IN ('totp', 'security_key', 'recovery_code', 'password'))
);

-- Two reads: resolve a presented token (via the unique index above) and revoke everything this
-- user holds. Partial, because a revoked or expired grant is only ever swept, never resolved.
CREATE INDEX step_up_grants_user_idx
  ON step_up_grants (user_id) WHERE revoked_at IS NULL;
CREATE INDEX step_up_grants_expiry_idx ON step_up_grants (expires_at);
