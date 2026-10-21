#!/usr/bin/env bash
# The Zitadel provisioner against the dev stack (`just up` first):
#   - no token, request body or password in a command's arguments (ps shows
#     those to anyone on the host);
#   - SMTP credentials: both or neither, put in Zitadel, and taken away again
#     when the settings drop them;
#   - the romulus-api token kept from run to run, replaced when Zitadel
#     refuses it (401), and the new one working.
# (A check that fails another way keeps the token: see the `case` in
# provision.sh; nothing here can make Zitadel answer that on demand.)
# Leaves the stack as `just up` provisions it.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"
compose=(docker compose -p "$(scripts/dev-env --project-name)")
dev_dir=${ROMULUS_DEV_DIR:-.dev}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

ok() { echo "✓ $*"; }
fail() { echo "✗ $*" >&2; exit 1; }
# provision [-e VAR=value…]: one run, its log in $work/log; its exit status.
provision() { "${compose[@]}" run --rm -T "$@" zitadel-provision >/dev/null 2>"$work/log"; }
token() { sed -n 's/^ROMULUS_ZITADEL_TOKEN=//p' "$dev_dir/zitadel/oidc.env"; }
# The user of the romulus SMTP provider, as Zitadel has it.
smtp_user() {
    "${compose[@]}" run --rm -T --entrypoint sh zitadel-provision -c '
        printf "Authorization: Bearer %s\n" "$(tr -d "\n" < /bootstrap/admin.pat)" > /tmp/h
        printf "{}" | curl -sS -X POST "$ZITADEL_URL/admin/v1/email/_search" -H "Host: $ZITADEL_HOST" \
            -H @/tmp/h -H "Content-Type: application/json" --data-binary @-' 2>/dev/null \
        | jq -r '[.result[] | select(.description == "romulus")][0] | .smtp.user // ""'
}

if grep -nE -- '--data "|Bearer \$|--arg [a-z]+ "\$(SMTP_PASSWORD|password|DEV_PASSWORD|pat|zitadel_token)"' \
        deploy/zitadel/provision.sh; then
    fail "provision.sh puts a secret in a command's arguments (above)"
fi
ok "no token, body or password in a command's arguments"

if provision -e ROMULUS_SMTP_USERNAME=check; then
    fail "a username without a password was accepted"
fi
grep -q "both or neither" "$work/log" || { cat "$work/log" >&2; fail "no 'both or neither' for a username alone"; }
ok "SMTP credentials: both or neither"

provision -e ROMULUS_SMTP_USERNAME=check -e ROMULUS_SMTP_PASSWORD=check-secret \
    || { cat "$work/log" >&2; fail "the provisioner failed with SMTP credentials"; }
[ "$(smtp_user)" = check ] || fail "Zitadel's SMTP user is $(smtp_user), not check"
provision || { cat "$work/log" >&2; fail "the provisioner failed without SMTP credentials"; }
[ "$(smtp_user)" = "" ] || fail "Zitadel kept the SMTP user $(smtp_user) once the settings dropped it"
ok "SMTP credentials put in Zitadel, then taken away (the password emptied too)"

before=$(token)
[ -n "$before" ] || fail "no ROMULUS_ZITADEL_TOKEN in $dev_dir/zitadel/oidc.env (run just up)"
provision || { cat "$work/log" >&2; fail "the provisioner failed"; }
[ "$(token)" = "$before" ] || fail "a working token was replaced"
ok "a working romulus-api token is kept"

sed -i.bak 's/^ROMULUS_ZITADEL_TOKEN=.*/ROMULUS_ZITADEL_TOKEN=not-a-token/' "$dev_dir/zitadel/oidc.env"
rm -f "$dev_dir/zitadel/oidc.env.bak"
provision || { cat "$work/log" >&2; fail "the provisioner failed with a refused token"; }
grep -q "no longer valid" "$work/log" || { cat "$work/log" >&2; fail "a refused token was not said replaced"; }
after=$(token)
[ -n "$after" ] && [ "$after" != not-a-token ] && [ "$after" != "$before" ] || fail "the refused token was not replaced"
scripts/check-zitadel-mail >/dev/null || fail "the new token does not work (scripts/check-zitadel-mail)"
ok "a token Zitadel refuses is replaced, and the new one deletes users"
