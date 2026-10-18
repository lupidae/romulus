-- A playground's roles and database (docs/playground.md, "Roles in a
-- playground"), run once by play-entrypoint.sh as the init superuser,
-- before the socket anyone else can reach exists.
--
--   play_owner    owns the schema: the service builds, seeds and runs
--                 migrations as it. No superuser powers, no CREATEROLE.
--   play_read     the console (everyone in phase 1): USAGE and SELECT on
--                 what play_owner creates, no CREATE, no TEMP, no large
--                 objects, no signalling other backends. Real privileges,
--                 not a parse of the SQL, which a function or EXECUTE
--                 could always fool.
--   play_service  the service's control session: cancels and terminates
--                 statements past their deadline (pg_signal_backend).
--
-- None of them holds pg_execute_server_program, pg_read_server_files or
-- pg_write_server_files; no untrusted language is installed.
\set ON_ERROR_STOP on
SET client_min_messages = warning;

CREATE ROLE play_owner LOGIN PASSWORD :'owner_password' CONNECTION LIMIT 4;
CREATE ROLE play_read LOGIN PASSWORD :'read_password' CONNECTION LIMIT 12;
CREATE ROLE play_service LOGIN PASSWORD :'service_password' CONNECTION LIMIT 2;
GRANT pg_signal_backend TO play_service;

-- The roles' settings (play_read's default_transaction_read_only, a
-- convenience: the privileges are the rule) are set by play-entrypoint.sh
-- on every start, with the passwords: a role can change both for itself.

REVOKE ALL ON DATABASE postgres FROM PUBLIC;
REVOKE ALL ON DATABASE template1 FROM PUBLIC;
CREATE DATABASE playground TEMPLATE template0 ENCODING 'UTF8' LOCALE 'C.UTF-8';
REVOKE ALL ON DATABASE playground FROM PUBLIC;
GRANT CONNECT ON DATABASE playground TO play_owner, play_read, play_service;
GRANT CREATE, TEMPORARY ON DATABASE playground TO play_owner;

\connect playground
SET client_min_messages = warning;

ALTER SCHEMA public OWNER TO play_owner;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO play_read;
-- Whatever play_owner creates later (schemas, tables, views, sequences)
-- is readable, and only readable, by play_read.
ALTER DEFAULT PRIVILEGES FOR ROLE play_owner GRANT USAGE ON SCHEMAS TO play_read;
ALTER DEFAULT PRIVILEGES FOR ROLE play_owner GRANT SELECT ON TABLES TO play_read;
ALTER DEFAULT PRIVILEGES FOR ROLE play_owner GRANT SELECT ON SEQUENCES TO play_read;

-- Large objects need no privilege to create: a read-only role could
-- otherwise write (and fill the disk) through them.
REVOKE EXECUTE ON FUNCTION
    lo_creat(integer), lo_create(oid), lo_from_bytea(oid, bytea),
    lo_import(text), lo_import(text, oid), lo_export(oid, text),
    lo_open(oid, integer), lo_put(oid, bigint, bytea), lowrite(integer, bytea),
    lo_unlink(oid), lo_truncate(integer, integer), lo_truncate64(integer, bigint)
FROM PUBLIC;

-- A role may signal its own backends: everyone shares play_read, so one
-- person could stop another's statements. Only the service signals.
REVOKE EXECUTE ON FUNCTION
    pg_cancel_backend(integer), pg_terminate_backend(integer, bigint)
FROM PUBLIC;
GRANT EXECUTE ON FUNCTION
    pg_cancel_backend(integer), pg_terminate_backend(integer, bigint)
TO play_service;
