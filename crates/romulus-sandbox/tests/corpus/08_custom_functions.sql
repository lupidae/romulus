-- The schema calls functions it does not define (they live in migrations
-- remus does not render): defaults, checks, generated columns, indexes,
-- policies and views all reach them.
CREATE SCHEMA util;

CREATE TABLE articles (
    id          bigint PRIMARY KEY DEFAULT util.next_id(),
    public_id   text NOT NULL DEFAULT gen_short_id(10),
    title       text NOT NULL,
    slug        text GENERATED ALWAYS AS (util.slugify(title)) STORED,
    author_id   uuid NOT NULL DEFAULT util.current_user_id(),
    tenant_id   integer NOT NULL DEFAULT util.current_tenant(),
    body        text NOT NULL DEFAULT '',
    email       text,
    CONSTRAINT articles_slug_valid CHECK (util.is_valid_slug(slug)),
    CONSTRAINT articles_email_valid CHECK (email IS NULL OR is_email(email))
);
CREATE UNIQUE INDEX articles_public_id_idx ON articles (public_id);
CREATE INDEX articles_email_idx ON articles (util.normalize_email(email));
CREATE INDEX articles_title_idx ON articles (util.unaccent_lower(title::varchar));

ALTER TABLE articles ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON articles TO app_user
    USING (tenant_id = util.current_tenant());
CREATE POLICY authors_edit ON articles FOR UPDATE TO app_user
    USING (author_id = util.current_user_id() OR util.has_role('editor'));

CREATE VIEW article_cards AS
    SELECT id, title, util.excerpt(body, 160) AS excerpt FROM articles;

ALTER TABLE articles ADD COLUMN reading_minutes integer;
ALTER TABLE articles ALTER COLUMN reading_minutes SET DEFAULT util.reading_time('');
