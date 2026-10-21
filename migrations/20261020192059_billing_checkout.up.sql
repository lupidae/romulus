-- One checkout at a time per organization: the session it last opened, and
-- the nonce (and price) its Stripe idempotency key is made of, kept when the
-- call fails so a retry gets the same session instead of a second one.
ALTER TABLE org_billing
    ADD COLUMN checkout_session_id text CONSTRAINT org_billing_checkout_session_id_length CHECK (length(checkout_session_id) <= 255),
    ADD COLUMN checkout_nonce text CONSTRAINT org_billing_checkout_nonce_length CHECK (length(checkout_nonce) <= 64),
    ADD COLUMN checkout_price text CONSTRAINT org_billing_checkout_price_length CHECK (length(checkout_price) <= 255);
COMMENT ON COLUMN org_billing.checkout_session_id IS
    'The Checkout Session the organization last opened; a new checkout expires every other open one first.';
COMMENT ON COLUMN org_billing.checkout_nonce IS
    'Part of the Stripe idempotency key of a checkout not yet answered (with checkout_price): the same request again gets the same session.';

ALTER TYPE audit_action ADD VALUE 'subscription_duplicate_cancelled';
