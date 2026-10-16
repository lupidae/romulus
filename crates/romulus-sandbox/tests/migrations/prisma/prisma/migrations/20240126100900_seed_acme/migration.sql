-- Created with `prisma migrate dev --create-only`, then edited: the demo
-- organization every environment starts with.
INSERT INTO "Organization" ("id", "name", "slug") VALUES ('org_acme', 'Acme', 'acme');
INSERT INTO "User" ("id", "organizationId", "email", "fullName", "role")
VALUES ('usr_admin', 'org_acme', 'admin@acme.test', 'Acme Admin', 'OWNER');
