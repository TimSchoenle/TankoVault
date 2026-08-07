-- Decision journals: why the automatic merge and the automatic sync did what they did, and
-- what it takes to undo it.
--
-- Both features act without a human in the loop, and both were observable only as a `tracing`
-- line that nothing kept. `merge_candidates` recorded a score and a bag of signal names for the
-- pairs it *queued* and nothing at all for the ones it merged; `sync_history` recorded a
-- three-field JSON blob for a pull and a push, nothing for a match, a no-op, an exclusion or a
-- failure to match. Neither could answer "why did this happen", and neither could be undone.

-- ---------------------------------------------------------------------------------------
-- merge_decisions — one row per pair the duplicate sweep or an operator decided
-- ---------------------------------------------------------------------------------------
-- Not one row per pair *examined*: the sweep re-scores hundreds of pairs an hour and re-reaching
-- the same conclusion is not a decision. Rows are written when something changed or nearly did —
-- a merge, a queue movement, a guard holding back an otherwise-automatic merge, an exhausted
-- budget. See `record_merge_decision`.
--
-- `left_id`/`right_id` carry **no** foreign key, for the same reason `series_merges.merged_id`
-- does not: after an automatic merge one of the two rows is gone, and outliving it is the entire
-- point of the table.
CREATE TABLE merge_decisions (
  id          uuid PRIMARY KEY,
  decided_at  timestamptz NOT NULL DEFAULT now(),
  -- Groups every decision of one sweep run, so a bad run can be read (and reverted) as a unit.
  -- NULL for an operator's one-off merge from the console.
  sweep_id    uuid,
  -- 'sweep_new' | 'sweep_requeue' | 'sweep_recheck' | 'operator'
  trigger     text NOT NULL,
  -- Named `actor_id`, not `actor`, so `repo_privacy.rs` enumerates this table from the live
  -- schema and forces a decision about whether it is exported. A column named around that guard
  -- is a table that silently escapes it.
  actor_id    uuid REFERENCES users(id) ON DELETE SET NULL,

  left_id     uuid NOT NULL,
  right_id    uuid NOT NULL,
  left_title  text NOT NULL,
  right_title text NOT NULL,

  -- What the scorer concluded: 'auto' | 'review' | 'distinct'.
  verdict     text NOT NULL,
  -- The stable slug of the rule that produced the verdict (`Adjudication::reason`). "Not merged"
  -- has four meanings and a score cannot distinguish them.
  reason      text NOT NULL,
  -- Guards that fired. Non-empty on a `review` row means the pair cleared the score and identity
  -- bar and was held back anyway, which is the near-miss an operator most wants to see.
  blocked_by  text[] NOT NULL DEFAULT '{}',
  -- What was actually done, which is not the verdict: a verdict of 'auto' becomes 'deferred' when
  -- the run's merge budget is spent, and 'review' becomes 'queued', 'reopened' or 'requeued'.
  outcome     text NOT NULL,
  survivor_id uuid,
  absorbed_id uuid,

  score       real  NOT NULL,
  signals     text[] NOT NULL DEFAULT '{}',
  -- The similarity the score started from, before any bonus or penalty.
  base_score  real  NOT NULL,
  -- `[{rule, delta, detail}]` — every term the scorer applied, in order. This is what makes a
  -- score auditable: 1.0 reached from a 0.42 trigram base plus an exact-title bonus plus a shared
  -- author is a different claim from 1.0 reached from a 0.97 base and nothing else.
  terms       jsonb NOT NULL DEFAULT '[]',
  -- Which title on each side matched, both sides' facts, and the survivor-choice weights.
  evidence    jsonb NOT NULL DEFAULT '{}',
  -- The thresholds and guards in force when the decision was taken. Without it a decision cannot
  -- be re-judged after someone changes the configuration.
  policy      jsonb NOT NULL DEFAULT '{}',

  -- Everything needed to put the catalogue back exactly as it was. NULL unless a merge actually
  -- happened. See `capture_merge_undo` / `revert_merge` in crates/db.
  undo          jsonb,
  reverted_at   timestamptz,
  reverted_by   uuid REFERENCES users(id) ON DELETE SET NULL,
  revert_reason text,
  -- An operator's judgement that the decision was wrong, whether or not it was reverted. Kept
  -- apart from the revert: a merge can be undone as a precaution and be correct, and a merge can
  -- be wrong and not worth undoing.
  flagged_at    timestamptz,
  flagged_by    uuid REFERENCES users(id) ON DELETE SET NULL,
  flag_reason   text,

  CONSTRAINT merge_decisions_pair_order CHECK (left_id < right_id),
  CONSTRAINT merge_decisions_verdict_check
    CHECK (verdict IN ('auto', 'review', 'distinct')),
  CONSTRAINT merge_decisions_outcome_check
    CHECK (outcome IN ('merged', 'queued', 'requeued', 'reopened', 'withdrawn',
                       'distinct', 'deferred', 'unchanged')),
  -- A merged row must say which id survived and which stopped existing, or the decision cannot
  -- be read after the fact and cannot be reverted.
  CONSTRAINT merge_decisions_merged_names_both
    CHECK (outcome <> 'merged' OR (survivor_id IS NOT NULL AND absorbed_id IS NOT NULL)),
  -- A revert is only meaningful for a decision that carries an undo journal.
  CONSTRAINT merge_decisions_revert_needs_undo
    CHECK (reverted_at IS NULL OR undo IS NOT NULL)
);

CREATE INDEX merge_decisions_recent_idx ON merge_decisions (decided_at DESC);
CREATE INDEX merge_decisions_pair_idx   ON merge_decisions (left_id, right_id);
CREATE INDEX merge_decisions_sweep_idx  ON merge_decisions (sweep_id) WHERE sweep_id IS NOT NULL;
-- The console's default view and the revert path both ask for "merges that can still be undone".
CREATE INDEX merge_decisions_revertible_idx ON merge_decisions (decided_at DESC)
  WHERE undo IS NOT NULL AND reverted_at IS NULL;
CREATE INDEX merge_decisions_flagged_idx ON merge_decisions (flagged_at DESC)
  WHERE flagged_at IS NOT NULL;

-- ---------------------------------------------------------------------------------------
-- sync_decisions — one row per per-series decision of one reconciliation run
-- ---------------------------------------------------------------------------------------
-- `sync_history` stays: it is the *user-facing* log of what changed on their shelf. This is the
-- operator-facing record of why, and it covers the cases history never did — the entries that
-- matched nothing, the series skipped as excluded, the fields both sides already agreed on, and
-- above all the title match that produced a mapping, which was written with no score, no signals
-- and no record of which title matched.
CREATE TABLE sync_decisions (
  id          uuid PRIMARY KEY,
  -- Groups one account reconciliation, so a run can be read as a unit and a bad one reverted
  -- wholesale.
  run_id      uuid NOT NULL,
  decided_at  timestamptz NOT NULL DEFAULT now(),
  user_id     uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  -- NULL for a remote entry that matched nothing: there is no local series to point at, and that
  -- is exactly the case worth recording.
  --
  -- `SET NULL`, not `CASCADE`: this is an audit journal, and a journal a deletion can erase is
  -- not one. A merge re-points these rows at the survivor before deleting the absorbed series,
  -- so the cascade is only reached by an actual series deletion — where losing the series is not
  -- a reason to lose the record of what was synced onto it.
  series_id   uuid REFERENCES series(id) ON DELETE SET NULL,
  provider    text NOT NULL,
  external_id text,

  -- 'match' | 'progress' | 'status' | 'series' | 'metadata'
  scope       text NOT NULL,
  -- 'matched' | 'unmatched' | 'pull' | 'push' | 'create_remote' | 'conflict' | 'noop'
  -- | 'skipped' | 'import_status' | 'enriched' | 'unmapped'
  action      text NOT NULL,
  -- The stable slug for *why*: 'existing_mapping', 'title_match_above_threshold',
  -- 'only_remote_changed', 'no_ancestor_disagreement', 'policy_remote_wins', …
  reason      text NOT NULL,
  -- The conflict policy in force, NULL where the decision did not consult one.
  policy      text,
  -- Whether anything was actually written. A run is mostly `applied = false`, and separating the
  -- two is what lets the console show "what changed" without hiding "what was considered".
  applied     boolean NOT NULL DEFAULT false,

  -- Values as text, not typed columns: one table covers a numeric progress and an enum status,
  -- and the alternative is either two tables or four nullable typed columns per field.
  local_before   text,
  local_after    text,
  remote_before  text,
  remote_after   text,
  -- The three-way merge's common ancestor. Without it a pull cannot be distinguished from a
  -- clobber after the fact.
  ancestor_local  text,
  ancestor_remote text,

  -- Set on a 'match' row: how confidently the remote entry was identified as this series.
  match_score   real,
  match_signals text[] NOT NULL DEFAULT '{}',
  -- Which titles matched, the scored terms, the runner-up, the provider's own metadata.
  evidence      jsonb NOT NULL DEFAULT '{}',

  reverted_at   timestamptz,
  reverted_by   uuid REFERENCES users(id) ON DELETE SET NULL,
  revert_reason text,
  flagged_at    timestamptz,
  flagged_by    uuid REFERENCES users(id) ON DELETE SET NULL,
  flag_reason   text,

  CONSTRAINT sync_decisions_scope_check
    CHECK (scope IN ('match', 'progress', 'status', 'series', 'metadata'))
);

CREATE INDEX sync_decisions_recent_idx ON sync_decisions (decided_at DESC);
CREATE INDEX sync_decisions_run_idx    ON sync_decisions (run_id);
CREATE INDEX sync_decisions_user_idx   ON sync_decisions (user_id, decided_at DESC);
CREATE INDEX sync_decisions_series_idx ON sync_decisions (series_id, decided_at DESC)
  WHERE series_id IS NOT NULL;
-- The console's default view: decisions that changed something and have not been undone.
CREATE INDEX sync_decisions_applied_idx ON sync_decisions (decided_at DESC)
  WHERE applied AND reverted_at IS NULL;
CREATE INDEX sync_decisions_flagged_idx ON sync_decisions (flagged_at DESC)
  WHERE flagged_at IS NOT NULL;

-- ---------------------------------------------------------------------------------------
-- sync_match_blocks — a title match an operator judged wrong, permanently
-- ---------------------------------------------------------------------------------------
-- Deleting a wrong `sync_mappings` row does not fix anything: the next reconciliation re-runs
-- the same title match against the same catalogue and writes the same row back. This is the
-- durable half — the resolver consults it before accepting a title match, and an operator
-- flagging a match wrong lands here.
--
-- Deployment-wide, with no `user_id`, because `sync_mappings` is: the series ⇆ external-id
-- correspondence is a property of the catalogue and the provider, not of whose list it was
-- observed on. A match that is wrong for one reader is wrong for every reader.
CREATE TABLE sync_match_blocks (
  provider    text NOT NULL,
  external_id text NOT NULL,
  series_id   uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  reason      text,
  created_by  uuid REFERENCES users(id) ON DELETE SET NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (provider, external_id, series_id)
);

-- The resolver's question is "may this external id attach to this series?", asked once per
-- unmatched entry per run; the primary key already answers it. This index answers the console's
-- opposite question — "what has been blocked for this series?" — which the primary key cannot.
CREATE INDEX sync_match_blocks_series_idx ON sync_match_blocks (series_id);

-- ---------------------------------------------------------------------------------------
-- A reverted merge is a reason to re-derive the recommender's rows
-- ---------------------------------------------------------------------------------------
-- Undoing a merge changes both series' tags and authors — the survivor loses what it absorbed and
-- the restored series gets its own back — so both need re-embedding, exactly as the merge itself
-- queued the survivor. `rec_repair_queue.reason` is a closed set, so the new cause has to be
-- admitted here or the revert transaction fails on the check constraint.
ALTER TABLE rec_repair_queue DROP CONSTRAINT rec_repair_queue_reason_check;
ALTER TABLE rec_repair_queue
  ADD CONSTRAINT rec_repair_queue_reason_check
  CHECK (reason IN ('merged', 'features_changed', 'merge_reverted'));
