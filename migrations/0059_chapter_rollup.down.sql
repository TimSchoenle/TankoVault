DROP TRIGGER chapter_rollup_sources_update ON series_sources;
DROP TRIGGER chapter_rollup_chapters_truncate ON chapters;
DROP TRIGGER chapter_rollup_chapters_update ON chapters;
DROP TRIGGER chapter_rollup_chapters_delete ON chapters;
DROP TRIGGER chapter_rollup_chapters_insert ON chapters;

DROP FUNCTION chapter_rollup_sources_updated();
DROP FUNCTION chapter_rollup_chapters_truncated();
DROP FUNCTION chapter_rollup_chapters_updated();
DROP FUNCTION chapter_rollup_chapters_deleted();
DROP FUNCTION chapter_rollup_chapters_inserted();
DROP FUNCTION chapter_rollup_rebuild(uuid[]);
DROP FUNCTION chapter_rollup_apply(uuid[], timestamptz[], int[]);

DROP TABLE chapter_rollup;
