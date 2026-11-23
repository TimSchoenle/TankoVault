-- Per-account failure counts for credential checks, so a guessing campaign spread across many
-- addresses still locks the account it targets. The IP-keyed rate limit cannot see that: a
-- botnet has an address per guess. `kind` keeps the password and the second factor apart, so
-- knowing the password does not reset the count on the factor.
CREATE TABLE auth_throttle (
  user_id           uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind              text NOT NULL CHECK (kind IN ('password', 'second_factor')),
  failures          integer NOT NULL DEFAULT 0,
  window_started_at timestamptz NOT NULL DEFAULT now(),
  -- How many times this check has locked; doubles the next lock, reset by a success.
  lockouts          integer NOT NULL DEFAULT 0,
  locked_until      timestamptz,
  PRIMARY KEY (user_id, kind)
);
