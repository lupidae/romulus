-- audit_action keeps 'contract_withdrawn': Postgres cannot drop an enum value.
DROP TABLE legal_notices;
DROP TYPE legal_document;
ALTER TABLE org_billing
    DROP COLUMN checkout_acceptance_id,
    DROP COLUMN withdrawn_at,
    DROP COLUMN started_at,
    DROP COLUMN acceptance_id;
DROP TABLE withdrawals;
DROP TRIGGER users_note_legal_end ON users;
DROP FUNCTION users_note_legal_end();
DROP TABLE legal_acceptances;
DROP FUNCTION legal_acceptances_are_kept();
DROP TYPE terms_edition;
DROP TYPE legal_acceptance_kind;
DROP TYPE buyer;
ALTER TABLE users DROP COLUMN locale;
DROP TYPE locale;
