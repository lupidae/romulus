-- "Update draft" merges three texts: the draft, the newer version, and the
-- text the draft is relative to. A new draft is relative to its base
-- version's text; after an update, to what the update left (the newer
-- version's content, in the draft's own words where it kept them).
ALTER TABLE drafts ADD COLUMN base_sql text;

COMMENT ON COLUMN drafts.base_sql IS
    'The text the draft''s SQL is relative to, set by "Update draft". NULL: its base version''s text (the source, or render_sql of a pushed model).';
