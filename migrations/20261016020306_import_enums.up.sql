-- Versions imported from migration files (docs/MVP.md, "Next: migration
-- files"). In a migration of their own: a new enum value cannot be used in
-- the transaction that adds it.
ALTER TYPE version_origin ADD VALUE 'import';
ALTER TYPE version_source_kind ADD VALUE 'migration';

-- The migration tools an import reads (romulus_sandbox::import::Format).
CREATE TYPE migration_format AS ENUM (
    'plain', 'sqlx', 'flyway', 'prisma', 'dbmate', 'golang_migrate', 'node_pg_migrate',
    'alembic', 'django', 'rails'
);
