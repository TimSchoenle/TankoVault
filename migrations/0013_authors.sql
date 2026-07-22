-- Authors: mirrors the tags/series_tags shape (design §1 goal metadata field, never
-- implemented until now). No role column — Author/Artist credits are merged into one list.
CREATE TABLE authors (
  id   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  slug text NOT NULL UNIQUE,
  name text NOT NULL
);
CREATE TABLE series_authors (
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  author_id uuid NOT NULL REFERENCES authors(id) ON DELETE CASCADE,
  PRIMARY KEY (series_id, author_id)
);
