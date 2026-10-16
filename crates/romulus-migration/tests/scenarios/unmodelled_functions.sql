-- Functions the schema calls but does not define (the model has no
-- functions): defaults and a policy on them, a column and a table added
-- that call them too.
CREATE TABLE accounts (
    id integer PRIMARY KEY,
    tenant integer DEFAULT util.current_tenant(),
    code text DEFAULT util.new_code()
);
ALTER TABLE accounts ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_rows ON accounts USING (tenant = util.current_tenant());
-- ==== new ====
CREATE TABLE accounts (
    id integer PRIMARY KEY,
    tenant integer DEFAULT util.current_tenant(),
    code text DEFAULT util.new_code(),
    owner integer DEFAULT util.current_tenant()
);
ALTER TABLE accounts ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_rows ON accounts USING (tenant = util.current_tenant());
CREATE TABLE notes (id integer PRIMARY KEY, code text NOT NULL DEFAULT util.new_code());
