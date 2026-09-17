-- Watchlist backup entries whose series this catalogue does not have (yet).
--
-- A backup is portable: its entries name a series by provider page, external tracker id and
-- title, never by uuid. Restoring one into a freshly wiped database, or into another deployment
-- that has not crawled every provider, finds most series missing. Those entries wait here, and
-- the control plane's pending-import sweep attaches each one once a scan brings its series in.
--
-- The identifiers are stored as parallel arrays rather than as the document's JSON, so a row does
-- not carry a second, independently versioned copy of the backup format. The CHECKs hold the
-- arrays parallel, and bound them to the limits `tankovault_domain::watchlist_transfer` applies
-- before anything is written.
CREATE TABLE watchlist_import_pending (
  id                     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id                uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- `PortableEntry::identity_key`: importing the same backup twice queues each entry once.
  identity_key           text NOT NULL,
  title                  text NOT NULL,
  alternative_titles     text[] NOT NULL DEFAULT '{}',
  source_providers       text[] NOT NULL DEFAULT '{}',
  source_urls            text[] NOT NULL DEFAULT '{}',
  source_paths           text[] NOT NULL DEFAULT '{}',
  -- Zero-based index into the source arrays of the pinned source, if any.
  pinned_source          smallint,
  external_trackers      text[] NOT NULL DEFAULT '{}',
  external_ids           text[] NOT NULL DEFAULT '{}',
  status                 watch_status NOT NULL,
  notify                 boolean NOT NULL,
  sync_excluded          boolean NOT NULL,
  added_at               timestamptz NOT NULL,
  last_read_whole_number numeric(10,4),
  last_read_part_number  numeric(10,4),
  -- The import's choices, kept so a later attachment honours what the reader asked for then.
  prefer_imported        boolean NOT NULL,
  match_titles           boolean NOT NULL,
  queued_at              timestamptz NOT NULL DEFAULT now(),
  last_attempt_at        timestamptz NOT NULL DEFAULT now(),
  attempts               integer NOT NULL DEFAULT 1,
  UNIQUE (user_id, identity_key),
  CONSTRAINT watchlist_import_pending_sources_parallel CHECK (
    cardinality(source_providers) = cardinality(source_urls)
    AND cardinality(source_urls) = cardinality(source_paths)
    AND cardinality(source_paths) <= 32),
  CONSTRAINT watchlist_import_pending_external_parallel CHECK (
    cardinality(external_trackers) = cardinality(external_ids)
    AND cardinality(external_ids) <= 8),
  CONSTRAINT watchlist_import_pending_pin_in_range CHECK (
    pinned_source IS NULL
    OR (pinned_source >= 0 AND pinned_source < cardinality(source_paths))),
  CONSTRAINT watchlist_import_pending_titles_bounded CHECK (cardinality(alternative_titles) <= 32)
);

-- The sweep takes the least recently tried rows first.
CREATE INDEX watchlist_import_pending_due_idx ON watchlist_import_pending (last_attempt_at);
