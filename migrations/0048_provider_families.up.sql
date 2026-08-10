-- Two more provider *families*.
--
-- `madara` earned its own `adapter_kind` because the theme is shared by enough sites that
-- onboarding one is a config row rather than a Rust module. MangaThemesia (the theme behind
-- most of the ex-Asura scanlator sites) and the Manganato/Mangakakalot clone family are the
-- same bargain: one default selector set in `tankovault_adapters`, and every site running it
-- costs a `providers` row carrying only its deviations.
--
-- `ALTER TYPE ... ADD VALUE` is transaction-safe here only because nothing in this migration
-- goes on to *use* either value — Postgres refuses a new enum label used in the same
-- transaction that created it, and sqlx runs each migration file inside one.
ALTER TYPE adapter_kind ADD VALUE IF NOT EXISTS 'mangathemesia';
ALTER TYPE adapter_kind ADD VALUE IF NOT EXISTS 'manganato';
