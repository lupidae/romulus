ALTER TABLE drafts
    DROP CONSTRAINT drafts_pending_base_whole,
    DROP CONSTRAINT drafts_pending_base_fkey,
    DROP COLUMN pending_base_sql,
    DROP COLUMN pending_base_version_id;
