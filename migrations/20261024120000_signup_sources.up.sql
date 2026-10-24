-- Where people came from when they signed up (docs/MVP.md "Signup sources
-- and analytics"): the campaign parameters and the referring site's origin
-- the sign-up's URLs carried (never kept in the browser), reported by the
-- app after signing in (`POST /api/me/signup-source`) or sent with a
-- password sign-up's form. Nothing else about the visitor: no address, no
-- page, no time of the visit. Deleted with the account. Operators read it
-- with `romulus-api signups` (CSV).
CREATE TABLE signup_sources (
    user_id uuid PRIMARY KEY REFERENCES users ON DELETE CASCADE,
    utm_source text CONSTRAINT signup_sources_utm_source_length
        CHECK (char_length(utm_source) BETWEEN 1 AND 200),
    utm_medium text CONSTRAINT signup_sources_utm_medium_length
        CHECK (char_length(utm_medium) BETWEEN 1 AND 200),
    utm_campaign text CONSTRAINT signup_sources_utm_campaign_length
        CHECK (char_length(utm_campaign) BETWEEN 1 AND 200),
    utm_content text CONSTRAINT signup_sources_utm_content_length
        CHECK (char_length(utm_content) BETWEEN 1 AND 200),
    referrer text CONSTRAINT signup_sources_referrer_length
        CHECK (char_length(referrer) BETWEEN 1 AND 300),
    created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE signup_sources IS
    'One row per account whose sign-up was told; every field NULL: a direct visit (or one whose URLs carried nothing).';
COMMENT ON COLUMN signup_sources.referrer IS
    'The referring site''s origin only (scheme, host, port), never its path or query; NULL when it was this site or none.';
COMMENT ON COLUMN signup_sources.created_at IS
    'When the sign-up told it (as the account was made), not when the visit happened.';

-- A password sign-up's source, from its form, until the emailed link makes
-- the account (signups::Source as JSON; the link is deleted a day after it
-- expires).
ALTER TABLE auth_links ADD COLUMN signup_source jsonb;
