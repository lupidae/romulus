-- ROMULUS_OPERATOR_IGNORE_DOMAINS (operator notifications): accounts at
-- these email domains, and organizations only they are members of, are left
-- out of the operators' notifications and the digest's counts. The domains
-- come from the configuration, lowercase; these say what is left out.
CREATE FUNCTION email_in_domains(email text, domains text[]) RETURNS boolean
    LANGUAGE sql STABLE PARALLEL SAFE
    RETURN EXISTS (
        SELECT FROM unnest(domains) AS d (domain),
            lower(normalize(split_part(email, '@', -1), NFC) COLLATE pg_unicode_fast) AS e (domain)
        WHERE strpos(email, '@') > 0
          AND (e.domain = d.domain OR right(e.domain, char_length(d.domain) + 1) = '.' || d.domain)
    );
COMMENT ON FUNCTION email_in_domains(text, text[]) IS
    'Whether the address is at one of the domains (lowercase) or under one (ops.example.com is under example.com), ignoring case: OperatorConfig::ignores in Rust.';

CREATE FUNCTION organization_in_domains(org uuid, domains text[]) RETURNS boolean
    LANGUAGE sql STABLE PARALLEL SAFE
    RETURN cardinality(domains) > 0
        AND EXISTS (SELECT FROM memberships m WHERE m.org_id = org)
        AND NOT EXISTS (
            SELECT FROM memberships m JOIN users u ON u.id = m.user_id
            WHERE m.org_id = org AND NOT email_in_domains(u.email, domains)
        );
COMMENT ON FUNCTION organization_in_domains(uuid, text[]) IS
    'Whether every member of the organization (it has some) has an address email_in_domains.';
