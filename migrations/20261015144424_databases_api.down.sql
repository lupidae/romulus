COMMENT ON COLUMN versions.message IS NULL;
ALTER TABLE versions DROP CONSTRAINT versions_message_length;
ALTER TABLE databases DROP CONSTRAINT databases_description_length;
ALTER TABLE databases DROP CONSTRAINT databases_slug_not_reserved;
