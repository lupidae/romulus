COMMENT ON COLUMN versions.layout IS 'Table positions and groups as the draft held them; shape owned by the collab module.';
ALTER TABLE versions ALTER COLUMN layout SET DEFAULT '{}';
COMMENT ON COLUMN versions.message IS NULL;
ALTER TABLE versions DROP CONSTRAINT versions_message_length;
ALTER TABLE databases DROP CONSTRAINT databases_description_length;
ALTER TABLE databases DROP CONSTRAINT databases_slug_not_reserved;
