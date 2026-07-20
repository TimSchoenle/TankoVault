-- Providers: the single source of truth for a site's domain + parsing config.
CREATE TABLE providers (
  id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  slug              text NOT NULL UNIQUE,
  name              text NOT NULL,
  base_url          text NOT NULL,               -- change here on domain migration
  adapter           adapter_kind NOT NULL,
  config            jsonb NOT NULL DEFAULT '{}',  -- selectors, pagination, latest-feed path
  state             provider_state NOT NULL DEFAULT 'active',
  politeness        jsonb NOT NULL DEFAULT '{}',  -- rps, concurrency, crawl_delay, user_agent
  robots_txt        text,                          -- cached
  robots_at         timestamptz,
  last_full_scan_at timestamptz,
  created_at        timestamptz NOT NULL DEFAULT now(),
  updated_at        timestamptz NOT NULL DEFAULT now()
);

-- Canonical works.
CREATE TABLE series (
  id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  canonical_title  text NOT NULL,
  normalized_title text NOT NULL,                 -- matching key (see tankovault_domain::normalize)
  description      text,
  cover_url        text,                           -- link only
  content_type     content_type NOT NULL DEFAULT 'unknown',
  status           series_status NOT NULL DEFAULT 'unknown',
  release_year     int,
  search_vec       tsvector GENERATED ALWAYS AS (
                     to_tsvector('simple', coalesce(canonical_title,'') || ' ' ||
                                           coalesce(description,''))
                   ) STORED,
  created_at       timestamptz NOT NULL DEFAULT now(),
  updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX series_search_gin ON series USING gin (search_vec);
CREATE INDEX series_title_trgm ON series USING gin (normalized_title gin_trgm_ops);
CREATE INDEX series_status_idx ON series (status);

CREATE TABLE series_titles (          -- alternative titles aid cross-provider matching
  series_id  uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  title      text NOT NULL,
  normalized text NOT NULL,
  PRIMARY KEY (series_id, normalized)
);
CREATE INDEX series_titles_trgm ON series_titles USING gin (normalized gin_trgm_ops);

CREATE TABLE tags (
  id   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  slug text NOT NULL UNIQUE,
  name text NOT NULL
);
CREATE TABLE series_tags (
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  tag_id    uuid NOT NULL REFERENCES tags(id)   ON DELETE CASCADE,
  PRIMARY KEY (series_id, tag_id)
);

-- The join: one canonical series can exist on many providers.
CREATE TABLE series_sources (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  series_id       uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  provider_id     uuid NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  source_path     text NOT NULL,                  -- RELATIVE path; resolve against base_url
  provider_title  text,                            -- as seen on the provider
  content_hash    bytea,                           -- hash of last-seen metadata+chapter list
  chapter_count   int NOT NULL DEFAULT 0,
  last_scanned_at timestamptz,
  state           provider_state NOT NULL DEFAULT 'active',
  UNIQUE (provider_id, source_path)
);
CREATE INDEX series_sources_series_idx ON series_sources (series_id);

CREATE TABLE chapters (
  id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  series_source_id uuid NOT NULL REFERENCES series_sources(id) ON DELETE CASCADE,
  number           numeric(10,4) NOT NULL,         -- supports 10.5, 10.1 etc.
  volume           int,
  title            text,
  path             text NOT NULL,                  -- RELATIVE link to the chapter page
  published_at     timestamptz,
  discovered_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (series_source_id, number)
);
CREATE INDEX chapters_source_idx ON chapters (series_source_id, number DESC);
CREATE INDEX chapters_discovered ON chapters (discovered_at DESC);
