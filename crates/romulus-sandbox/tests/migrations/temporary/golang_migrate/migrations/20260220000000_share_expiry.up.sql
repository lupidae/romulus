ALTER TABLE share_links ADD COLUMN expires_at timestamptz;
COMMENT ON COLUMN share_links.granted_at IS 'When the link was granted (backfilled from the activity log).';
