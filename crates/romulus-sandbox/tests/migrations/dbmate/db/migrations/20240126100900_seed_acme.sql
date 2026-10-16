-- migrate:up
-- The demo organization every environment starts with.
INSERT INTO organizations (name, slug) VALUES ('Acme', 'acme');

INSERT INTO users (organization_id, email, full_name, role)
SELECT id, 'admin@acme.test', 'Acme Admin', 'owner'
FROM organizations
WHERE slug = 'acme';

-- migrate:down
DELETE FROM organizations WHERE slug = 'acme';
