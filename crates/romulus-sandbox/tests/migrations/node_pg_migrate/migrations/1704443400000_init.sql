-- Up Migration
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS citext;

CREATE TYPE user_role AS ENUM ('owner', 'admin', 'member');

CREATE TABLE organizations (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    slug text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    email citext NOT NULL UNIQUE,
    full_name text,
    role user_role NOT NULL DEFAULT 'member',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX users_organization_id_idx ON users (organization_id);

-- Down Migration
DROP TABLE users;
DROP TABLE organizations;
DROP TYPE user_role;
