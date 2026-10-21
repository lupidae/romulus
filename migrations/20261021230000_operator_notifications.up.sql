-- Operator notifications (docs/MVP.md "Operator notifications"): what the
-- people running an install hear about as it happens, and a daily digest,
-- by email to ROMULUS_OPERATOR_EMAILS, through the mail outbox
-- (mail_outbox). Both tables stay empty while that variable is unset.
-- One digest a day: the row is taken before the digest is queued.
CREATE TABLE operator_digests (
    day date PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    mrr jsonb NOT NULL DEFAULT '{}',
    playground_month date,
    playground_seconds bigint
);
COMMENT ON TABLE operator_digests IS
    'The daily digests queued (operator::digest), by UTC day, with what the next one compares with: the monthly recurring revenue in cents per currency, the playground time of a month.';

-- A plan limit an organization reached, told once in 30 days per cap.
CREATE TABLE operator_limit_notices (
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    cap text NOT NULL CONSTRAINT operator_limit_notices_cap_length CHECK (length(cap) <= 40),
    notified_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, cap)
);
COMMENT ON TABLE operator_limit_notices IS
    'When the operators were last told that an organization reached a cap of its plan (a 402 plan_limit): again only 30 days later.';
