DROP TRIGGER series_browse_tags_update ON series_tags;
DROP TRIGGER series_browse_tags_delete ON series_tags;
DROP TRIGGER series_browse_tags_insert ON series_tags;
DROP TRIGGER series_browse_sources_update ON series_sources;
DROP TRIGGER series_browse_sources_delete ON series_sources;
DROP TRIGGER series_browse_sources_insert ON series_sources;
DROP TRIGGER series_browse_series_update ON series;
DROP TRIGGER series_browse_series_insert ON series;

DROP FUNCTION series_browse_tags_changed();
DROP FUNCTION series_browse_sources_changed();
DROP FUNCTION series_browse_series_changed();
DROP FUNCTION series_browse_rebuild(uuid[]);
DROP FUNCTION series_browse_refresh_tags(uuid);
DROP FUNCTION series_browse_refresh_sources(uuid);

DROP TABLE series_browse;
