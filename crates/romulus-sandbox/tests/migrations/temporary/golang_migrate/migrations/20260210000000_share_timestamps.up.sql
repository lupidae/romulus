-- Links and grants learn when they were granted and last used, backfilled
-- from the activity log through a temporary helper table.
ALTER TABLE share_links ADD COLUMN granted_at timestamptz;
ALTER TABLE share_links ADD COLUMN last_used_at timestamptz;
ALTER TABLE share_grants ADD COLUMN granted_at timestamptz;

CREATE TEMP TABLE link_first_seen AS
SELECT resource_type,
       resource_id,
       min(created_at) AS first_seen,
       max(created_at) AS last_seen
FROM activity_log
WHERE resource_type IN ('link', 'grant')
GROUP BY resource_type, resource_id;

CREATE INDEX ON link_first_seen (resource_type, resource_id);
ANALYZE link_first_seen;

UPDATE share_links l
SET granted_at = d.first_seen,
    last_used_at = d.last_seen
FROM link_first_seen d
WHERE d.resource_type = 'link' AND d.resource_id = l.id;

DROP TABLE link_first_seen;

-- Grants: the same, from the grant events.
CREATE TEMPORARY TABLE link_first_seen ON COMMIT DROP AS
SELECT resource_id, min(created_at) AS first_seen
FROM activity_log
WHERE resource_type = 'grant'
GROUP BY resource_id;

UPDATE share_grants g
SET granted_at = d.first_seen
FROM link_first_seen d
WHERE d.resource_id = g.id;

DROP TABLE IF EXISTS link_first_seen;

UPDATE share_links SET granted_at = created_at WHERE granted_at IS NULL;
UPDATE share_grants SET granted_at = created_at WHERE granted_at IS NULL;
ALTER TABLE share_links ALTER COLUMN granted_at SET DEFAULT now();
ALTER TABLE share_links ALTER COLUMN granted_at SET NOT NULL;
ALTER TABLE share_grants ALTER COLUMN granted_at SET DEFAULT now();
ALTER TABLE share_grants ALTER COLUMN granted_at SET NOT NULL;
CREATE INDEX share_links_last_used_at_idx ON share_links (last_used_at);
