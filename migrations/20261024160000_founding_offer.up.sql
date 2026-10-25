-- The founding-team offer (billing::plans::FOUNDING, Terms 7.11): Team on its
-- own prices, the first 20 organizations, once each.
ALTER TABLE org_billing
    ADD COLUMN founding boolean NOT NULL DEFAULT false,
    ADD COLUMN checkout_offer_until timestamptz,
    ADD COLUMN checkout_offer_by uuid REFERENCES users (id) ON DELETE SET NULL;

COMMENT ON COLUMN org_billing.founding IS
    'The stored subscription is on a founding-team price (Team at the offer''s price): amount_cents is that price, and no price change of the plan table applies to it.';
COMMENT ON COLUMN org_billing.checkout_offer_until IS
    'The organization''s checkout is on a founding-team price and holds one of the 20 places until then: a day past the session''s expiry (a late webhook of a payment is still counted), released when the session expires or is cancelled, or a live subscription is stored.';
COMMENT ON COLUMN org_billing.checkout_offer_by IS
    'Who started the checkout holding a founding-team place: one place held per person at a time.';

CREATE INDEX org_billing_checkout_offer_until ON org_billing (checkout_offer_until)
    WHERE checkout_offer_until IS NOT NULL;

-- Every organization that had a subscription on a founding-team price: the
-- places taken, and once per organization. No foreign key on purpose: a
-- deleted organization keeps its place taken (only its id and the time).
CREATE TABLE founding_offer_uses (
    org_id uuid PRIMARY KEY,
    used_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE founding_offer_uses IS
    'The organizations that had a subscription on a founding-team price (Terms 7.11): the 20 places taken, once per organization. Kept when the organization is deleted (no foreign key): its place stays taken.';
