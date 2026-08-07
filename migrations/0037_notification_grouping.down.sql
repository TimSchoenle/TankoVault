DROP INDEX IF EXISTS notifications_open_group_idx;
ALTER TABLE notifications DROP COLUMN IF EXISTS group_key;
