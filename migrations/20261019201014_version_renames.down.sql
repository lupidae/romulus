-- The versions forget the renames they were published with: migrations
-- between them detect renames afresh.
ALTER TABLE versions DROP CONSTRAINT versions_renames_is_an_object;
ALTER TABLE versions DROP COLUMN renames;
