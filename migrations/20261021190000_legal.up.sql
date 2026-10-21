-- The hosted service's contract (docs/MVP.md "Legal: terms, checkout,
-- withdrawal"): the language a person's contract and billing emails are in,
-- the record of every acceptance of the Terms (at the first sign-in, and at
-- each checkout with what the buyer stated), consumers' withdrawals, and the
-- change notices sent. Like billing, every table stays empty unless the
-- Terms are configured (ROMULUS_TERMS_VERSION): a self-hosted install never
-- writes them.
CREATE TYPE locale AS ENUM ('en', 'fr');
ALTER TABLE users ADD COLUMN locale locale;
COMMENT ON COLUMN users.locale IS
    'The language of the person''s contract and billing emails, chosen when accepting the Terms and changed in account settings. NULL: never chosen (English for emails; the app follows the browser).';

CREATE TYPE buyer AS ENUM ('business', 'consumer');
CREATE TYPE legal_acceptance_kind AS ENUM ('terms', 'checkout');
-- Which Terms are in force: the professional edition while romulus sells to
-- businesses only (ROMULUS_CONSUMER_SALES off), the full one once consumers
-- may buy too.
CREATE TYPE terms_edition AS ENUM ('professional', 'full');

CREATE TABLE legal_acceptances (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    kind legal_acceptance_kind NOT NULL,
    accepted_at timestamptz NOT NULL DEFAULT now(),
    user_id uuid REFERENCES users ON DELETE SET NULL,
    user_email text NOT NULL CONSTRAINT legal_acceptances_user_email_length CHECK (length(user_email) <= 320),
    user_name text NOT NULL CONSTRAINT legal_acceptances_user_name_length CHECK (length(user_name) <= 500),
    account_deleted_at timestamptz,
    terms_version text NOT NULL CONSTRAINT legal_acceptances_terms_version_length CHECK (length(terms_version) <= 64),
    edition terms_edition NOT NULL,
    locale locale NOT NULL,
    -- The professional edition's: the person attests using romulus for their
    -- trade, business or profession.
    professional_use boolean,
    -- A checkout's: the organization (no foreign key, the record outlives
    -- it), the plan, as whom, and the boxes ticked with the wording's version.
    org_id uuid,
    org_name text CONSTRAINT legal_acceptances_org_name_length CHECK (length(org_name) <= 500),
    plan billing_plan,
    interval billing_interval,
    buyer buyer,
    company_name text CONSTRAINT legal_acceptances_company_name_length CHECK (length(company_name) <= 200),
    company_country text CONSTRAINT legal_acceptances_company_country_code CHECK (company_country ~ '^[A-Z]{2}$'),
    company_tax_id text CONSTRAINT legal_acceptances_company_tax_id_length CHECK (length(company_tax_id) <= 40),
    business_confirmed boolean,
    immediate_start boolean,
    withdrawal_acknowledged boolean,
    copy_version text CONSTRAINT legal_acceptances_copy_version_length CHECK (length(copy_version) <= 64),
    checkout_session_id text CONSTRAINT legal_acceptances_checkout_session_id_length CHECK (length(checkout_session_id) <= 255),
    CONSTRAINT legal_acceptances_checkout_complete CHECK (
        kind = 'terms'
        OR (org_id IS NOT NULL AND org_name IS NOT NULL AND plan IS NOT NULL AND interval IS NOT NULL
            AND buyer IS NOT NULL AND copy_version IS NOT NULL AND checkout_session_id IS NOT NULL)
    ),
    CONSTRAINT legal_acceptances_business_stated CHECK (
        buyer IS DISTINCT FROM 'business'
        OR (business_confirmed AND company_name IS NOT NULL AND company_country IS NOT NULL)
    ),
    CONSTRAINT legal_acceptances_consumer_requested CHECK (
        buyer IS DISTINCT FROM 'consumer' OR (immediate_start AND withdrawal_acknowledged)
    ),
    CONSTRAINT legal_acceptances_professional_only CHECK (
        edition = 'full' OR (kind = 'terms' AND professional_use) OR (kind = 'checkout' AND buyer = 'business')
    )
);
COMMENT ON TABLE legal_acceptances IS
    'Proof of the contract (Terms 3.6): who accepted which Terms, when, in which language, what they attested, and at a checkout what they stated. Not tied to the organization; never changed; purged by legal::purge: under the professional edition 5 years after the account ends; under the full one, checkouts 10 years after (Article L213-1 of the French Consumer Code), sign-ins 5 years after the account ends.';
COMMENT ON COLUMN legal_acceptances.kind IS
    'terms: the box at sign-in (the Free plan''s contract, or a new version of the Terms); checkout: the boxes before paying.';
COMMENT ON COLUMN legal_acceptances.user_email IS 'The address as it was: the record outlives the account.';
COMMENT ON COLUMN legal_acceptances.account_deleted_at IS 'When the account was deleted (a trigger on users): the end of the contract, from which a sign-in record is kept 5 years.';
COMMENT ON COLUMN legal_acceptances.edition IS
    'The Terms accepted: the professional edition (businesses only) or the full one; a change of edition is accepted again, like a new version.';
COMMENT ON COLUMN legal_acceptances.copy_version IS
    'Which wording of the boxes was ticked (billing::copy, checkout-boxes.json: a version per edition).';
COMMENT ON COLUMN legal_acceptances.professional_use IS
    'The professional edition: the person attested using romulus for professional purposes, not as a consumer.';
COMMENT ON COLUMN legal_acceptances.checkout_session_id IS 'The Stripe Checkout Session opened for it; the subscription carries this record''s id in its metadata.';

CREATE INDEX legal_acceptances_user_idx ON legal_acceptances (user_id, accepted_at DESC) WHERE user_id IS NOT NULL;
CREATE INDEX legal_acceptances_org_idx ON legal_acceptances (org_id) WHERE org_id IS NOT NULL;
CREATE INDEX legal_acceptances_accepted_idx ON legal_acceptances (accepted_at);

-- What a record says never changes: only the account's departure is noted.
CREATE FUNCTION legal_acceptances_are_kept() RETURNS trigger
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
CREATE TRIGGER legal_acceptances_are_kept BEFORE UPDATE ON legal_acceptances
    FOR EACH ROW EXECUTE FUNCTION legal_acceptances_are_kept();

-- A deleted account ends its contract: its records say when (before the
-- foreign key forgets whose they were).
CREATE FUNCTION users_note_legal_end() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE legal_acceptances SET account_deleted_at = now() WHERE user_id = OLD.id;
    RETURN OLD;
END
$$;
CREATE TRIGGER users_note_legal_end BEFORE DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION users_note_legal_end();

CREATE TABLE withdrawals (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    requested_at timestamptz NOT NULL DEFAULT now(),
    org_id uuid NOT NULL,
    org_name text NOT NULL CONSTRAINT withdrawals_org_name_length CHECK (length(org_name) <= 500),
    user_id uuid REFERENCES users ON DELETE SET NULL,
    user_email text NOT NULL CONSTRAINT withdrawals_user_email_length CHECK (length(user_email) <= 320),
    user_name text NOT NULL CONSTRAINT withdrawals_user_name_length CHECK (length(user_name) <= 500),
    locale locale NOT NULL,
    acceptance_id uuid REFERENCES legal_acceptances ON DELETE SET NULL,
    subscription_id text NOT NULL UNIQUE CONSTRAINT withdrawals_subscription_id_length CHECK (length(subscription_id) <= 255),
    plan billing_plan NOT NULL,
    interval billing_interval NOT NULL,
    subscribed_at timestamptz NOT NULL,
    paid_cents bigint NOT NULL CONSTRAINT withdrawals_paid_positive CHECK (paid_cents >= 0),
    refund_cents bigint NOT NULL CONSTRAINT withdrawals_refund_within_paid CHECK (refund_cents >= 0 AND refund_cents <= paid_cents),
    currency text NOT NULL CONSTRAINT withdrawals_currency_length CHECK (length(currency) <= 10),
    refunds jsonb NOT NULL DEFAULT '[]',
    completed_at timestamptz
);
COMMENT ON TABLE withdrawals IS
    'A consumer''s withdrawal from a paid plan within 14 days (Terms 9.4, Articles L221-18 and L221-21 of the French Consumer Code): recorded when asked, then the subscription cancelled and the unused part refunded (completed_at). Not tied to the organization; purged 10 years after.';
COMMENT ON COLUMN withdrawals.refunds IS
    'The refunds planned when it was asked, one per paid invoice: {"invoice", "payment_intent", "amount"}; each is made once (Stripe idempotency keys by invoice).';
COMMENT ON COLUMN withdrawals.completed_at IS
    'When Stripe had cancelled the subscription and made every refund; NULL: the notices task tries again.';
CREATE INDEX withdrawals_org_idx ON withdrawals (org_id);
CREATE INDEX withdrawals_pending_idx ON withdrawals (requested_at) WHERE completed_at IS NULL;

ALTER TABLE org_billing
    ADD COLUMN acceptance_id uuid REFERENCES legal_acceptances ON DELETE SET NULL,
    ADD COLUMN started_at timestamptz,
    ADD COLUMN withdrawn_at timestamptz,
    ADD COLUMN checkout_acceptance_id uuid;
COMMENT ON COLUMN org_billing.acceptance_id IS
    'The checkout record of the stored subscription (from its metadata): who bought it, as a business or a consumer.';
COMMENT ON COLUMN org_billing.started_at IS 'When the stored subscription started: a consumer may withdraw for 14 days from it.';
COMMENT ON COLUMN org_billing.withdrawn_at IS
    'A consumer withdrew from the stored subscription: the organization is on Free from then on, whatever Stripe still says.';
COMMENT ON COLUMN org_billing.checkout_acceptance_id IS
    'The record id the pending checkout carries in its metadata, kept with checkout_nonce so a retry sends Stripe the same request.';
CREATE INDEX org_billing_acceptance_idx ON org_billing (acceptance_id) WHERE acceptance_id IS NOT NULL;

CREATE TYPE legal_document AS ENUM ('terms', 'privacy', 'dpa');
CREATE TABLE legal_notices (
    document legal_document NOT NULL,
    version text NOT NULL CONSTRAINT legal_notices_version_length CHECK (length(version) <= 64),
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    sent_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (document, version, user_id)
);
CREATE INDEX legal_notices_user_idx ON legal_notices (user_id);
COMMENT ON TABLE legal_notices IS
    'Change notices sent (romulus-api legal-notice: Terms and Privacy Policy to everyone, the DPA to admins), once per person and version.';

ALTER TYPE audit_action ADD VALUE IF NOT EXISTS 'contract_withdrawn';
