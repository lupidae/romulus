-- A discount off every period of a subscription, for as long as it runs (a
-- coupon of duration forever, such as the founding-team offer's): what each
-- period costs is amount_cents less it. amount_cents stays the price's own
-- amount, which price changes compare with the plan table.
ALTER TABLE org_billing ADD COLUMN discount_cents bigint
    CONSTRAINT org_billing_discount_cents_check CHECK (discount_cents > 0);

COMMENT ON COLUMN org_billing.discount_cents IS
    'Off every period, for as long as the subscription runs (a forever coupon: the founding-team offer); what a period costs is amount_cents less it. NULL: none.';

-- The founding-team offer: once per organization, and a place held while an
-- offered checkout is open.
ALTER TABLE org_billing
    ADD COLUMN founding_offer_used_at timestamptz,
    ADD COLUMN checkout_offer_until timestamptz;

COMMENT ON COLUMN org_billing.founding_offer_used_at IS
    'When a subscription with the founding-team offer was first stored: the organization''s one use; it never gets the offer again.';
COMMENT ON COLUMN org_billing.checkout_offer_until IS
    'The organization''s open checkout applies the founding-team offer and holds one of its places until then (the session''s expires_at); cleared once a subscription is stored.';

CREATE INDEX org_billing_checkout_offer_until ON org_billing (checkout_offer_until)
    WHERE checkout_offer_until IS NOT NULL;
