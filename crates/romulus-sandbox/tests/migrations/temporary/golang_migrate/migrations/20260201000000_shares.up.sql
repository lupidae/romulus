-- The sharing: links to resources, grants, and their activity log.
CREATE TABLE activity_log (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    resource_type text NOT NULL,
    resource_id uuid NOT NULL,
    action text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX activity_log_resource_idx ON activity_log (resource_type, resource_id);

CREATE TABLE share_links (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    resource_type text NOT NULL,
    resource_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE share_grants (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    link_id uuid NOT NULL REFERENCES share_links (id) ON DELETE CASCADE,
    grantee text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
