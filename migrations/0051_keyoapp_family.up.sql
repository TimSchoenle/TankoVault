-- One more provider *family*: Keyoapp, the hosted platform a cluster of scanlator sites runs on.
--
-- Same bargain as `mangathemesia`/`manganato` in 0048: one default selector set in
-- `tankovault_adapters`, and every site running the platform costs a `providers` row carrying
-- only its deviations rather than a Rust module of its own.
--
-- `ALTER TYPE ... ADD VALUE` is transaction-safe here only because nothing in this migration
-- goes on to *use* the value — Postgres refuses a new enum label used in the same transaction
-- that created it, and sqlx runs each migration file inside one.
ALTER TYPE adapter_kind ADD VALUE IF NOT EXISTS 'keyoapp';
