-- Providers themselves survive: only the link to the preset catalogue and the catalogue
-- mirror go. A rolled-back deployment keeps every row it installed, unmanaged, exactly as the
-- releases before this one had them.
ALTER TABLE providers
  DROP CONSTRAINT IF EXISTS providers_lock_needs_preset,
  DROP COLUMN IF EXISTS preset_slug,
  DROP COLUMN IF EXISTS preset_locked,
  DROP COLUMN IF EXISTS preset_synced_at;

DROP TABLE IF EXISTS provider_presets;
