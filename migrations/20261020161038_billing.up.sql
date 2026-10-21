-- Hosted billing (docs/MVP.md "Billing"): an organization's subscription as
-- Stripe has it, the webhook events already handled, playground time used per
-- month, and the emails sent once (renewal reminders, payment notices). Every
-- table stays empty unless billing is configured (ROMULUS_BILLING): a
-- self-hosted install never writes them, and nothing is capped there.
CREATE TYPE billing_plan AS ENUM ('free', 'starter', 'team', 'business');
CREATE TYPE billing_interval AS ENUM ('month', 'year');

CREATE TABLE org_billing (
    org_id uuid PRIMARY KEY REFERENCES organizations ON DELETE CASCADE,
    customer_id text UNIQUE CONSTRAINT org_billing_customer_id_length CHECK (length(customer_id) <= 255),
    subscription_id text UNIQUE CONSTRAINT org_billing_subscription_id_length CHECK (length(subscription_id) <= 255),
    plan billing_plan NOT NULL DEFAULT 'free',
    interval billing_interval,
    status text CONSTRAINT org_billing_status_length CHECK (length(status) <= 40),
    current_period_end timestamptz,
    cancel_at_period_end boolean NOT NULL DEFAULT false,
    amount_cents bigint,
    currency text CONSTRAINT org_billing_currency_length CHECK (length(currency) <= 10),
    past_due_since timestamptz,
    scheduled_plan billing_plan,
    scheduled_interval billing_interval,
    scheduled_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE org_billing IS
    'An organization''s subscription as Stripe last said (re-read on every webhook). No row, or no live subscription: the Free plan.';
COMMENT ON COLUMN org_billing.plan IS
    'The plan the subscription pays for; the plan in force is computed from it, the status and the grace period (billing::state).';
COMMENT ON COLUMN org_billing.status IS
    'Stripe''s subscription status (active, trialing, past_due, unpaid, canceled, incomplete, incomplete_expired, paused).';
COMMENT ON COLUMN org_billing.past_due_since IS
    'The due date of the first unpaid invoice while past_due or unpaid: the plan holds for 14 days from it, then the organization is on Free.';
COMMENT ON COLUMN org_billing.scheduled_plan IS
    'A change Stripe will make at scheduled_at (a downgrade at the end of the period).';

CREATE TABLE billing_events (
    id text PRIMARY KEY CONSTRAINT billing_events_id_length CHECK (length(id) <= 255),
    type text NOT NULL CONSTRAINT billing_events_type_length CHECK (length(type) <= 100),
    org_id uuid REFERENCES organizations ON DELETE SET NULL,
    received_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX billing_events_org_idx ON billing_events (org_id) WHERE org_id IS NOT NULL;
CREATE INDEX billing_events_received_idx ON billing_events (received_at);
COMMENT ON TABLE billing_events IS
    'Stripe webhook events handled, by id: a delivery seen again is acknowledged and not applied twice. Kept 90 days.';

CREATE TABLE playground_usage (
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    month date NOT NULL CONSTRAINT playground_usage_month_first CHECK (extract(day FROM month) = 1),
    seconds bigint NOT NULL DEFAULT 0 CONSTRAINT playground_usage_seconds_positive CHECK (seconds >= 0),
    PRIMARY KEY (org_id, month)
);
COMMENT ON TABLE playground_usage IS
    'Running playground time per organization and calendar month (UTC), migration tests'' playgrounds included: the plan''s playground hours.';

CREATE TABLE billing_notices (
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    kind text NOT NULL CONSTRAINT billing_notices_kind_length CHECK (length(kind) <= 40),
    about text NOT NULL CONSTRAINT billing_notices_about_length CHECK (length(about) <= 255),
    sent_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, kind, about)
);
COMMENT ON TABLE billing_notices IS
    'Emails sent once: a renewal reminder per period, a payment notice per invoice, playground hours warnings per month.';

-- Versions older than the plan's history window are hidden, never deleted;
-- the latest version and a version an open draft is based on stay visible.
CREATE FUNCTION version_kept_visible(p_version uuid, p_database uuid) RETURNS boolean
LANGUAGE sql STABLE PARALLEL SAFE AS $$
    SELECT EXISTS (SELECT 1 FROM drafts d WHERE d.base_version_id = p_version AND d.status = 'open')
        OR p_version = (SELECT v.id FROM versions v
                        WHERE v.database_id = p_database AND v.removed_at IS NULL
                        ORDER BY v.number DESC LIMIT 1)
$$;
COMMENT ON FUNCTION version_kept_visible(uuid, uuid) IS
    'Whether a version stays visible whatever the plan''s history window: the database''s latest live version, or the base of an open draft.';

CREATE INDEX drafts_open_base_idx ON drafts (base_version_id) WHERE status = 'open';
CREATE INDEX versions_created_idx ON versions (database_id, created_at);

ALTER TYPE audit_action ADD VALUE 'terms_accepted';
ALTER TYPE audit_action ADD VALUE 'subscription_changed';
ALTER TYPE audit_action ADD VALUE 'payment_failed';
ALTER TYPE audit_action ADD VALUE 'payment_succeeded';
ALTER TYPE audit_action ADD VALUE 'plan_downgraded';
ALTER TYPE audit_action ADD VALUE 'playground_hours_used_up';
