-- Provider presets become data, and a provider row can be governed by one.
--
-- Until now the shipped presets existed only as Rust (`tankovault_adapters::builtin_presets`)
-- and the install job could only *create* from them. A preset fix therefore reached new
-- installations and nothing else, and an existing deployment had no way to see that its row
-- had drifted from the definition the build ships.

-- The catalogue the install job records on every rollout. It is a mirror of the build's
-- compiled-in presets, not operator state: `bootstrap seed-providers` rewrites it wholesale
-- and deletes entries the build no longer ships.
--
-- It exists because the api tier deliberately does not depend on `tankovault-adapters` (that
-- would pull BoringSSL into the api image — see services/api/Cargo.toml), so the console
-- cannot read the presets from code. Writing them down once per rollout is what lets every
-- other tier treat them as ordinary rows.
CREATE TABLE provider_presets (
  slug        text PRIMARY KEY,
  name        text NOT NULL,
  base_url    text NOT NULL,
  adapter     adapter_kind NOT NULL,
  config      jsonb NOT NULL DEFAULT '{}',
  -- What a new provider from this preset starts at. Never re-applied to an existing row:
  -- politeness is outside the lock, deliberately (see `providers.preset_locked`).
  politeness  jsonb NOT NULL DEFAULT '{}',
  updated_at  timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE providers
  -- Soft reference, with no FK to `provider_presets` on purpose: a build that stops shipping a
  -- preset must leave the deployment's provider — and its whole catalogue — standing. The link
  -- then dangles, which is exactly what the console reports.
  ADD COLUMN preset_slug      text,
  -- While true, the install job overwrites this row's name, base_url, adapter and config from
  -- the preset. Never its politeness or state: those are the operator's answer to their own
  -- infrastructure and legal position, and a rollout that silently restored a rate limit
  -- somebody had lowered would be a worse bug than a stale selector.
  ADD COLUMN preset_locked    boolean NOT NULL DEFAULT false,
  ADD COLUMN preset_synced_at timestamptz,
  ADD CONSTRAINT providers_lock_needs_preset
    CHECK (preset_slug IS NOT NULL OR NOT preset_locked);

-- No backfill, and none is possible here: whether an existing row still matches the preset it
-- came from is a question about the *shipped* definitions, which live in Rust. `bootstrap
-- seed-providers` adopts on its next run — locking rows that still equal their preset, and
-- marking the ones an operator has edited as customised rather than overwriting them.
