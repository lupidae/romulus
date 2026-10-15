-- Database pages live at /app/<org>/<database>, next to the organization's
-- own pages: a slug must not shadow one of them (the API refuses them too,
-- `databases::slug::RESERVED`; keep both lists equal).
ALTER TABLE databases ADD CONSTRAINT databases_slug_not_reserved
    CHECK (slug NOT IN ('settings', 'new', 'members', 'invitations', 'search', 'activity'));

-- The API keeps both short; these are the backstops.
ALTER TABLE databases ADD CONSTRAINT databases_description_length
    CHECK (char_length(description) <= 1000);
ALTER TABLE versions ADD CONSTRAINT versions_message_length
    CHECK (message IS NULL OR (btrim(message) <> '' AND char_length(message) <= 1000));

COMMENT ON COLUMN versions.message IS 'What changed, in a line (the uploader''s or the pusher''s). NULL when none was given.';

-- One layout shape everywhere (versions::layout::VersionLayout).
ALTER TABLE versions ALTER COLUMN layout SET DEFAULT '{"layout": {}, "groups": {}}';
COMMENT ON COLUMN versions.layout IS
    'Table positions and groups: {"layout": {"<schema>.<table>": {"x", "y"}}, "groups": {"<id>": {"name", "color", "members": [qualified names]}}}, pruned to the model''s tables. An empty object reads as an empty layout.';
