-- Required extensions.
--   citext   : case-insensitive email/username uniqueness.
--   pg_trgm  : trigram similarity for fuzzy title matching (design §10).
-- UUIDv7 ids are generated in the application layer (uuid crate) and passed
-- explicitly on every insert, giving time-sortable, index-friendly primary keys.
-- The column DEFAULTs below use gen_random_uuid() only as a fallback for manual
-- or seed inserts; production rows always carry app-supplied UUIDv7 values.
CREATE EXTENSION IF NOT EXISTS citext;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
