-- The hosted contract, reviewed: when a checkout's contract ends and the
-- business Stripe's page named (legal_acceptances), a withdrawal's attempts
-- at Stripe (withdrawals), the boxes ticked at a password sign-up that
-- finishes by email (auth_links), and the outbox every contract and billing
-- letter goes through (mail_outbox).

ALTER TABLE legal_acceptances
    ADD COLUMN contract_ended_at timestamptz,
    ADD COLUMN stripe_business_name text
        CONSTRAINT legal_acceptances_stripe_business_name_length CHECK (length(stripe_business_name) <= 500);
COMMENT ON COLUMN legal_acceptances.contract_ended_at IS
    'A checkout''s: when the contract it concluded ended (the subscription over for good, a withdrawal, the organization deleted), from which the record is kept 5 years. NULL: running, or never concluded.';
COMMENT ON COLUMN legal_acceptances.stripe_business_name IS
    'A checkout''s: the business named on Stripe''s payment page (checkout.session.completed), the Customer of Terms 3.5 c).';
COMMENT ON TABLE legal_acceptances IS
    'Proof of the contract (Terms 3.6): who accepted which Terms, when, in which language, what they attested, and at a checkout what they stated. Not tied to the organization; never changed; purged by legal::purge: a sign-in''s 5 years after the account ends, a checkout''s 5 years after its contract ended and the account ended (under the full edition, 10 years after it was concluded at least: Article L213-1 of the French Consumer Code).';

-- What a record says never changes: only the account's departure, the end
-- of the contract and the business Stripe's page named are noted, once.
CREATE OR REPLACE FUNCTION legal_acceptances_are_kept() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW) - 'user_id' - 'account_deleted_at' - 'contract_ended_at' - 'stripe_business_name')
            <> (to_jsonb(OLD) - 'user_id' - 'account_deleted_at' - 'contract_ended_at' - 'stripe_business_name')
        OR (NEW.user_id IS NOT NULL AND NEW.user_id IS DISTINCT FROM OLD.user_id)
        OR (OLD.contract_ended_at IS NOT NULL AND NEW.contract_ended_at IS DISTINCT FROM OLD.contract_ended_at)
        OR (OLD.stripe_business_name IS NOT NULL
            AND NEW.stripe_business_name IS DISTINCT FROM OLD.stripe_business_name) THEN
        RAISE EXCEPTION 'legal acceptance % is immutable', OLD.id
            USING ERRCODE = 'restrict_violation', CONSTRAINT = 'legal_acceptances_are_kept';
    END IF;
    RETURN NEW;
END
$$;

-- A deleted organization ends the contracts its checkouts concluded.
CREATE FUNCTION organizations_note_contract_end() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE legal_acceptances SET contract_ended_at = now()
    WHERE org_id = OLD.id AND kind = 'checkout' AND contract_ended_at IS NULL;
    RETURN OLD;
END
$$;
CREATE TRIGGER organizations_note_contract_end BEFORE DELETE ON organizations
    FOR EACH ROW EXECUTE FUNCTION organizations_note_contract_end();

ALTER TABLE withdrawals
    ADD COLUMN attempts integer NOT NULL DEFAULT 0 CONSTRAINT withdrawals_attempts_positive CHECK (attempts >= 0),
    ADD COLUMN failed_at timestamptz,
    ADD COLUMN last_error text CONSTRAINT withdrawals_last_error_length CHECK (length(last_error) <= 2000);
COMMENT ON COLUMN withdrawals.attempts IS 'How many times Stripe was asked to cancel and refund.';
COMMENT ON COLUMN withdrawals.failed_at IS
    'When romulus stopped asking Stripe (too many attempts, or a week): the operator finishes it by hand (logged as an error).';

ALTER TABLE auth_links
    ADD COLUMN accepted_terms jsonb
        CONSTRAINT auth_links_accepted_terms_sign_up CHECK (accepted_terms IS NULL OR purpose = 'sign_up');
COMMENT ON COLUMN auth_links.accepted_terms IS
    'A sign-up''s: the Terms accepted on its form ({version, edition, locale, professional_use, copy_version}), recorded when the account is created.';

CREATE TABLE mail_outbox (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    key text NOT NULL UNIQUE CONSTRAINT mail_outbox_key_length CHECK (length(key) <= 500),
    kind text NOT NULL CONSTRAINT mail_outbox_kind_length CHECK (length(kind) <= 64),
    recipient text NOT NULL CONSTRAINT mail_outbox_recipient_length CHECK (length(recipient) <= 320),
    subject text NOT NULL,
    text_body text NOT NULL,
    html_body text NOT NULL,
    attachments jsonb NOT NULL DEFAULT '[]',
    created_at timestamptz NOT NULL DEFAULT now(),
    attempts integer NOT NULL DEFAULT 0 CONSTRAINT mail_outbox_attempts_positive CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    failed_at timestamptz,
    last_error text CONSTRAINT mail_outbox_last_error_length CHECK (length(last_error) <= 2000)
);
COMMENT ON TABLE mail_outbox IS
    'Contract and billing letters (mail::outbox): queued in the transaction that decides them, sent by a worker that marks a letter sent only once the mail server accepted it, and tries again until it gives up (failed_at, logged as an error). One row per key: a letter never goes twice. Sent rows are deleted after 30 days, given-up ones after 90.';
COMMENT ON COLUMN mail_outbox.key IS 'What the letter is, once: e.g. billing:<org>:subscription_confirmed:<subscription>:<address>.';
COMMENT ON COLUMN mail_outbox.attachments IS 'The files it carries, by name (embedded in the binary: legal::pdf): [{"filename", "content_type"}].';
COMMENT ON COLUMN mail_outbox.next_attempt_at IS 'When it may go (again); a worker sending it holds it a few minutes ahead.';
CREATE INDEX mail_outbox_due_idx ON mail_outbox (next_attempt_at) WHERE sent_at IS NULL AND failed_at IS NULL;
CREATE INDEX mail_outbox_created_at_idx ON mail_outbox (created_at);
