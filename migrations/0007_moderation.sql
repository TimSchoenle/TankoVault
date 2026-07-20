-- Canonicalisation review queue (design §10): when the matcher lands in the
-- ambiguous confidence band it creates the source but flags a merge candidate for
-- operator review (one-click merge/split in the console).
CREATE TABLE merge_candidates (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  series_id     uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  candidate_id  uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  score         real NOT NULL,                      -- match confidence [0,1]
  reason        text,
  resolved      boolean NOT NULL DEFAULT false,
  resolved_by   uuid REFERENCES users(id) ON DELETE SET NULL,
  resolved_at   timestamptz,
  created_at    timestamptz NOT NULL DEFAULT now(),
  CHECK (series_id <> candidate_id)
);
CREATE INDEX merge_candidates_open ON merge_candidates (created_at DESC) WHERE NOT resolved;

-- Structured audit log for privileged actions (design §16): provider edits, domain
-- migrations, merges/splits, scan triggers. Append-only; never updated.
CREATE TABLE audit_log (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  actor_id   uuid REFERENCES users(id) ON DELETE SET NULL,
  action     text NOT NULL,                         -- 'provider.update','series.merge', ...
  target     text,                                  -- affected entity id/description
  detail     jsonb NOT NULL DEFAULT '{}',
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_actor_idx  ON audit_log (actor_id, created_at DESC);
CREATE INDEX audit_log_action_idx ON audit_log (action, created_at DESC);
