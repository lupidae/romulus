DROP TABLE mail_outbox;
ALTER TABLE auth_links DROP COLUMN accepted_terms;
ALTER TABLE withdrawals
    DROP COLUMN last_error,
    DROP COLUMN failed_at,
    DROP COLUMN attempts;
DROP TRIGGER organizations_note_contract_end ON organizations;
DROP FUNCTION organizations_note_contract_end();
CREATE OR REPLACE FUNCTION legal_acceptances_are_kept() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW) - 'user_id' - 'account_deleted_at') <> (to_jsonb(OLD) - 'user_id' - 'account_deleted_at')
        OR (NEW.user_id IS NOT NULL AND NEW.user_id IS DISTINCT FROM OLD.user_id) THEN
        RAISE EXCEPTION 'legal acceptance % is immutable', OLD.id
            USING ERRCODE = 'restrict_violation', CONSTRAINT = 'legal_acceptances_are_kept';
    END IF;
    RETURN NEW;
END
$$;
ALTER TABLE legal_acceptances
    DROP COLUMN stripe_business_name,
    DROP COLUMN contract_ended_at;
COMMENT ON TABLE legal_acceptances IS
    'Proof of the contract (Terms 3.6): who accepted which Terms, when, in which language, what they attested, and at a checkout what they stated. Not tied to the organization; never changed; purged by legal::purge: under the professional edition 5 years after the account ends; under the full one, checkouts 10 years after (Article L213-1 of the French Consumer Code), sign-ins 5 years after the account ends.';
