-- RLS switched on and off, forced; policies added, altered (roles, using,
-- check), recreated (command), dropped; comments everywhere.
CREATE TYPE level AS ENUM ('low', 'high');
CREATE DOMAIN positive AS integer CHECK (VALUE > 0);
CREATE TABLE docs (id integer PRIMARY KEY, owner text, level level, size positive, CONSTRAINT docs_size_check CHECK (size < 1000));
CREATE TABLE secrets (id integer PRIMARY KEY);
ALTER TABLE secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
CREATE POLICY docs_read ON docs FOR SELECT TO app_user USING (owner = current_user);
CREATE POLICY docs_write ON docs FOR UPDATE USING (true) WITH CHECK (true);
CREATE POLICY docs_old ON docs FOR DELETE USING (false);
CREATE POLICY docs_kind ON docs FOR SELECT USING (true);
COMMENT ON TABLE docs IS 'Documents';
COMMENT ON COLUMN docs.owner IS 'Who wrote it';
COMMENT ON TYPE level IS 'How secret';
-- ==== new ====
CREATE TYPE level AS ENUM ('low', 'high');
CREATE DOMAIN positive AS integer CHECK (VALUE > 0);
CREATE TABLE docs (id integer PRIMARY KEY, owner text, level level, size positive, CONSTRAINT docs_size_check CHECK (size < 1000));
CREATE TABLE secrets (id integer PRIMARY KEY);
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
ALTER TABLE docs FORCE ROW LEVEL SECURITY;
CREATE POLICY docs_read ON docs FOR SELECT TO app_user, app_admin USING (owner = current_user OR level = 'low');
CREATE POLICY docs_write ON docs FOR UPDATE USING (owner = current_user) WITH CHECK (owner = current_user);
CREATE POLICY docs_kind ON docs AS RESTRICTIVE FOR ALL USING (true);
CREATE POLICY docs_new ON docs FOR INSERT WITH CHECK (true);
COMMENT ON TABLE docs IS 'Documents, versioned';
COMMENT ON COLUMN docs.id IS 'Stable';
COMMENT ON CONSTRAINT docs_size_check ON docs IS 'Keep them small';
COMMENT ON TYPE level IS NULL;
COMMENT ON DOMAIN positive IS 'Above zero';
COMMENT ON TABLE secrets IS 'No policies';
