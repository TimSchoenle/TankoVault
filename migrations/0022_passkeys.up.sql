-- Passkeys: WebAuthn discoverable credentials as a first-class sign-in credential.
--
-- ---------------------------------------------------------------------------------------
-- Two tables, because a ceremony is not a credential
-- ---------------------------------------------------------------------------------------
-- WebAuthn is a two-leg protocol. The server issues a challenge, the authenticator signs it,
-- and the server verifies the signature *against the challenge it issued* — so the challenge
-- state has to survive between two unrelated HTTP requests, and it has to be consumed exactly
-- once. `user_passkeys` holds the durable credential; `webauthn_ceremonies` holds the
-- in-flight challenge and is deleted on use.
--
-- The ceremony state is in Postgres rather than in Redis or in a cookie:
--
--   * **Not Redis.** Redis is optional in this deployment (`main.rs::connect_redis` degrades
--     to per-process state with a warning). Rate-limit counters and SSE tickets can survive
--     that degradation; an authentication ceremony cannot — behind two api replicas, the
--     `finish` leg would land on the replica that never saw the `start` leg and sign-in would
--     fail roughly half the time, intermittently, with nothing to point at. Postgres is the
--     one dependency readiness already requires.
--   * **Not a cookie.** The client must not hold the state it is being verified against. The
--     `PasskeyRegistration`/`DiscoverableAuthentication` values carry the challenge and the
--     policy the response is checked under; handing them to the client and taking them back
--     means the attacker chooses both halves of the comparison. webauthn-rs says this in
--     capitals on every one of those types.
--
-- ---------------------------------------------------------------------------------------
-- Why the credential is an opaque JSON document
-- ---------------------------------------------------------------------------------------
-- `credential` is `webauthn_rs::prelude::Passkey` as serialised by the library. Its fields —
-- the COSE public key, the signature counter, the backup-eligibility and backup-state flags,
-- the parsed attestation — are the library's business and it evolves them; a column per field
-- would be this schema asserting a shape it does not own, and the first upstream addition
-- would silently drop whatever the columns did not cover. The one field lifted out is the
-- credential id, because it is the *lookup key* (below) and a lookup key inside a JSON blob
-- is an unindexable sequential scan.
--
-- ---------------------------------------------------------------------------------------
-- Why `credential_id` is globally unique, not unique per user
-- ---------------------------------------------------------------------------------------
-- Both because the spec says so and because the alternative is an account-takeover primitive.
-- A discoverable sign-in presents a credential id and a user handle, and the server resolves
-- the account from them; if two accounts could hold the same credential id the resolution is
-- ambiguous, which is the same shape of bug `0021_username_not_an_email` closed for logins.
-- Worse, it lets an attacker who has observed a victim's credential id register it against
-- their own account and have the victim's authenticator resolve there. `UNIQUE` on the whole
-- table makes the second registration a `23505` the API turns into `409`, whichever account
-- attempts it.
--
-- `bytea`, not `text`: a credential id is an opaque byte string chosen by the authenticator,
-- and base64url-encoding it before storage would make equality depend on the padding
-- convention of whichever encoder wrote the row.
CREATE TABLE user_passkeys (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id       uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The authenticator's opaque handle for this credential. See the header.
  credential_id bytea NOT NULL UNIQUE,
  -- Serialised `webauthn_rs::prelude::Passkey`. Rewritten wholesale after an authentication
  -- that advances the signature counter or changes a backup flag.
  credential    jsonb NOT NULL,
  -- What the user calls this key ("Phone", "YubiKey 5C"). Free text, never interpreted; it
  -- exists so the revoke list is meaningful when someone holds four of them.
  label         text NOT NULL,
  created_at    timestamptz NOT NULL DEFAULT now(),
  -- NULL until the key is first used to sign in. Surfaced so a user can tell a live key from
  -- one they registered on a laptop they no longer own.
  last_used_at  timestamptz
);

-- The list view: every key for one account, newest first. Covers the ownership-scoped reads
-- and the ownership check on rename/delete.
CREATE INDEX user_passkeys_user_idx ON user_passkeys (user_id, created_at DESC);

-- An in-flight WebAuthn ceremony: one row between `start` and `finish`, deleted on use.
--
-- `user_id` is NULL for a sign-in ceremony and set for a registration ceremony. That is not a
-- nullable field for convenience — it is the whole point of discoverable authentication: the
-- server issues the sign-in challenge *without knowing who is signing in*, and learns the
-- account only from the user handle the authenticator returns. A registration ceremony, by
-- contrast, is bound to the account that started it, and `finish` refuses a row whose
-- `user_id` is not the caller's, so a leaked ceremony id cannot install a credential on
-- someone else's account.
--
-- `ON DELETE CASCADE` so an erasure (GDPR Art. 17) does not trip the foreign key on a
-- ceremony the user happened to have open.
CREATE TABLE webauthn_ceremonies (
  id         uuid PRIMARY KEY,
  user_id    uuid REFERENCES users(id) ON DELETE CASCADE,
  -- 'register' | 'authenticate'. Text rather than an enum: this is a private, short-lived
  -- implementation detail of one module, and a new enum type is schema surface that outlives
  -- it. Checked below so a typo in a query cannot write an unknown kind.
  kind       text NOT NULL,
  -- The serialised `PasskeyRegistration` / `DiscoverableAuthentication`. Opaque, for the same
  -- reason `user_passkeys.credential` is.
  state      jsonb NOT NULL,
  expires_at timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT webauthn_ceremony_kind CHECK (kind IN ('register', 'authenticate'))
);

-- Drives the expiry sweep. Every ceremony is consumed or expires within minutes, so this
-- table stays tiny — but only if something actually deletes the abandoned rows, and an
-- abandoned row is the common case (a user who closes the browser mid-prompt).
CREATE INDEX webauthn_ceremonies_expiry_idx ON webauthn_ceremonies (expires_at);
