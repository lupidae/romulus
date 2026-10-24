-- Where people came from before signing up (docs/MVP.md "Signup sources"):
-- the first visit's campaign parameters and referring site, as the browser
-- kept them (first touch, 30 days), reported once by the app after sign-up
-- (`POST /api/me/signup-source`). Nothing else about the visitor: no address,
-- no page, no time of the visit. Deleted with the account. Operators read it
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
    'One row per account whose sign-up the app reported; every field NULL: a direct visit (or one the browser did not keep).';
COMMENT ON COLUMN signup_sources.referrer IS
    'The referring site''s origin only (scheme, host, port), never its path or query; NULL when it was this site or none.';
COMMENT ON COLUMN signup_sources.created_at IS
    'When the app reported it (shortly after the account was made), not when the visit happened.';
