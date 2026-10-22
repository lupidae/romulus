# What deleting an account erases

When someone asks for their data to go, you run `delete-user`
([configuration.md](configuration.md#retention-and-deletion)). This page
says what that erases, what stays and where, and how to remove what stays.

## In romulus

`delete-user` (and, when it is on, the deletion of inactive accounts) erases
from romulus's database, in one transaction:

- the account: name, email address, password, sessions, sign-in devices,
  emailed links, access tokens, connected apps, memberships, approvals,
  unread markers;
- every organization the account is the only member of, with all it holds.

What stays, and for how long:

| What | Where | Until |
|---|---|---|
| What the person wrote in organizations they shared (versions, drafts, comments) | romulus's database | the organization deletes it; shown as by a deleted account |
| Their name in the audit log of organizations they shared | `audit_events` | the organization is deleted |
| Their address and IP address in the sign-in log (sign-ins, password resets, the deletion itself) | `auth_events` | 90 days |
| Emails to them already queued or sent (address, subject, text) | the mail outbox, `mail_outbox` | 30 days after sent, 90 after given up |
| Everything | your backups ([backup.md](backup.md)) | each backup expires |
| Operator notifications about them | your operators' mailboxes | your mail policy |
| Request logs | wherever you keep the API's output | your log policy |

## In Zitadel, when it is your identity provider

With a Zitadel OIDC client and `ROMULUS_ZITADEL_TOKEN`, `delete-user` also
deletes the person's Zitadel user (`DELETE /v2/users/{id}`); without the
token it prints the user to delete in Zitadel's console. Either way, Zitadel
then removes the user from every table it reads (its projections): profile,
email, login names, sessions, grants, memberships. Nobody can sign in as
them any more, and the address is free to sign up again.

**What stays.** Zitadel is event-sourced: its event store
(`eventstore.events2` in Zitadel's database) keeps every event as written,
and deleting a user only adds a `user.removed` event without data. We
checked on Zitadel v4.19.4: after the deletion, the event store still
holds, for that user,

- `user.human.added`: the username, first, last, nick and display names,
  the email address, the phone number and the password hash;
- the events of every later change (email, phone, profile, address,
  password, metadata, links to other identity providers);
- one event per sign-in check (`user.human.password.check.succeeded`, and
  the other checks): the IP address, the user agent and the languages;
- their sessions (`oidc_session.added`, `session.added`): the user agent,
  and for API sessions the IP address and request headers.

Zitadel's administrators can read these through its Events API and
console, and Zitadel's own backups keep them. Its `AuditLogRetention`
setting only hides old events from that API; it deletes nothing.

**Zitadel has no supported way to erase them.** No API, command or
documented procedure does it in v4.19.4; Zitadel tracks it in
[zitadel#807](https://github.com/zitadel/zitadel/issues/807) and
[zitadel#7811](https://github.com/zitadel/zitadel/issues/7811).

**How to remove them anyway.** The SQL below blanks those fields (sets them
to JSON `null`) in the events of every user Zitadel removed and of their
sessions. It changes no other column, deletes no row and leaves every other
user alone, so Zitadel can still replay its events (it rebuilds its tables
from them). Running it again changes nothing more. It is not supported by
Zitadel: we tested it on v4.19.4 (the removed users' data gone from every
table, projections rebuilt from the events without error, the address
signing up again); re-test it on a copy after each Zitadel upgrade, since
new events can bring new fields.

1. Back up Zitadel's database.
2. Delete the user (`delete-user`, or Zitadel's console).
3. Run the script against Zitadel's database, as its owner:

   ```bash
   psql "$ZITADEL_DATABASE_URL" -v ON_ERROR_STOP=1 -f redact-removed-users.sql
   ```

4. Backups taken before still hold the data until they expire.

```sql
-- redact-removed-users.sql: what Zitadel's event store keeps of removed users.
BEGIN;

-- Every user Zitadel removed.
CREATE TEMPORARY TABLE removed_users ON COMMIT DROP AS
SELECT DISTINCT instance_id, aggregate_id AS user_id
FROM eventstore.events2
WHERE aggregate_type = 'user' AND event_type = 'user.removed';

-- Their own events: username, names, email, phone, postal address, password
-- hash, secrets, metadata values, the account at an external identity
-- provider, and each sign-in check's IP address, user agent and languages.
UPDATE eventstore.events2 e
SET payload = (
  SELECT jsonb_object_agg(p.key, CASE WHEN p.key = ANY (k.keys) THEN 'null'::jsonb ELSE p.value END)
  FROM jsonb_each(e.payload) p
)
FROM removed_users r,
     (VALUES (ARRAY[
       'userName', 'username', 'firstName', 'lastName', 'nickName', 'displayName',
       'email', 'phone', 'country', 'locality', 'postalCode', 'region',
       'streetAddress', 'encodedHash', 'secret', 'otpSecret', 'value', 'userId',
       'remoteIP', 'userAgent', 'acceptLanguage'
     ])) AS k (keys)
WHERE e.instance_id = r.instance_id
  AND e.aggregate_type = 'user' AND e.aggregate_id = r.user_id
  AND jsonb_typeof(e.payload) = 'object'
  AND EXISTS (SELECT 1 FROM jsonb_each(e.payload) p
              WHERE p.key = ANY (k.keys) AND p.value <> 'null'::jsonb);

-- Their sessions (API, OIDC, SAML): the user agent (IP address, headers)
-- and the session's metadata.
UPDATE eventstore.events2 e
SET payload = (
  SELECT jsonb_object_agg(p.key, CASE WHEN p.key = ANY (k.keys) THEN 'null'::jsonb ELSE p.value END)
  FROM jsonb_each(e.payload) p
)
FROM (SELECT DISTINCT s.instance_id, s.aggregate_type, s.aggregate_id
      FROM eventstore.events2 s
      JOIN removed_users r ON r.instance_id = s.instance_id AND s.payload ->> 'userID' = r.user_id
      WHERE s.aggregate_type IN ('session', 'oidc_session', 'saml_session')) AS s,
     (VALUES (ARRAY['user_agent', 'userAgent', 'metadata'])) AS k (keys)
WHERE e.instance_id = s.instance_id
  AND e.aggregate_type = s.aggregate_type AND e.aggregate_id = s.aggregate_id
  AND jsonb_typeof(e.payload) = 'object'
  AND EXISTS (SELECT 1 FROM jsonb_each(e.payload) p
              WHERE p.key = ANY (k.keys) AND p.value <> 'null'::jsonb);

COMMIT;
```

What the script leaves: the user's id, their preferred language, the times
of their events, which events happened (created, verified, signed in,
removed), the ids of the organizations, projects and applications they
used, and the scopes they were granted. None of it names the person on its
own: romulus's deleted account held the link between the id and the
address.

Another identity provider (your own OIDC server) has its own retention:
delete the user there, and check what its logs and history keep.
