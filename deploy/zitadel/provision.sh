#!/usr/bin/env bash
# Makes the dev Zitadel instance usable by romulus, with no clicking:
#   project "romulus" → OIDC web app "romulus-web" → test users alice and bob.
# Then writes the contract other tools read:
#   $OUT/oidc.env    ROMULUS_OIDC_ISSUER / _CLIENT_ID / _CLIENT_SECRET
#   $OUT/users.json  [{email, password, name}]
#
# It runs on every `compose up`, so every step finds what exists before
# creating anything. Zitadel never shows a client secret twice: when the app
# exists but oidc.env is gone (or names another client), the secret is
# regenerated rather than guessed.
set -euo pipefail

: "${ZITADEL_URL:?}"          # http://zitadel:8080, inside the compose network
: "${ZITADEL_HOST:?}"         # localhost:<port>: Zitadel picks the instance, and the issuer, from Host
: "${ROMULUS_PUBLIC_URL:?}"   # the web origin; Vite proxies /api to the API
: "${ROMULUS_API_PORT:?}"
: "${ROMULUS_PREVIEW_PORT:?}" # the production build Playwright drives, with its own API
OUT=${OUT:-/out}
PAT_FILE=${PAT_FILE:-/bootstrap/admin.pat}
OWNER=${OWNER:-}
DEV_PASSWORD=${DEV_PASSWORD:-RomulusDev1!}

issuer="http://$ZITADEL_HOST"
# Two browser origins per stack: the dev server ($ROMULUS_PUBLIC_URL) and the
# preview build end-to-end tests use; plus the API port, for direct testing.
preview="http://localhost:$ROMULUS_PREVIEW_PORT"
redirect_uris=$(jq -cn --arg web "$ROMULUS_PUBLIC_URL" --arg preview "$preview" \
    --arg api "http://localhost:$ROMULUS_API_PORT" \
    '[$web, $preview, $api] | map(. + "/api/auth/callback")')
# The SPA lives under /app (the API serves the homepage at / in production).
post_logout_uris=$(jq -cn --arg web "$ROMULUS_PUBLIC_URL" --arg preview "$preview" \
    '[$web, $preview] | map(. + "/app/signin", . + "/app/", . + "/")')

log() { echo "zitadel-provision: $*" >&2; }

# The PAT is written by Zitadel's first-instance setup, which finishes before
# the container reports healthy; the wait only covers a slow volume.
for _ in $(seq 1 30); do [ -s "$PAT_FILE" ] && break; sleep 1; done
[ -s "$PAT_FILE" ] || { log "no PAT at $PAT_FILE (was the zitadel-bootstrap volume recreated without the database?)"; exit 1; }
pat=$(tr -d '\n' < "$PAT_FILE")

# api METHOD PATH [JSON] → response body on stdout. A non-2xx is fatal, with
# Zitadel's message, unless the body carries the error id in $TOLERATE.
api() {
    local method=$1 path=$2 body=${3:-} response status
    response=$(curl -sS -X "$method" "$ZITADEL_URL$path" \
        -H "Host: $ZITADEL_HOST" \
        -H "Authorization: Bearer $pat" \
        -H 'Content-Type: application/json' \
        ${body:+--data "$body"} \
        -w '\n%{http_code}')
    status=${response##*$'\n'}
    response=${response%$'\n'*}
    if [ "${status:0:1}" != 2 ] && { [ -z "${TOLERATE:-}" ] || [[ $response != *"$TOLERATE"* ]]; }; then
        log "$method $path → $status: $response"
        return 1
    fi
    printf '%s' "$response"
}

# Project
project_id=$(api POST /management/v1/projects/_search \
    '{"queries":[{"nameQuery":{"name":"romulus","method":"TEXT_QUERY_METHOD_EQUALS"}}]}' \
    | jq -r '.result[0].id // empty')
if [ -z "$project_id" ]; then
    project_id=$(api POST /management/v1/projects '{"name":"romulus"}' | jq -r .id)
    log "created project romulus ($project_id)"
fi

# OIDC app. The config is reapplied on every run, so a changed slot (ports)
# only needs another `up`.
oidc_config=$(jq -cn --argjson redirects "$redirect_uris" --argjson logouts "$post_logout_uris" '{
    redirectUris: $redirects,
    postLogoutRedirectUris: $logouts,
    responseTypes: ["OIDC_RESPONSE_TYPE_CODE"],
    grantTypes: ["OIDC_GRANT_TYPE_AUTHORIZATION_CODE", "OIDC_GRANT_TYPE_REFRESH_TOKEN"],
    appType: "OIDC_APP_TYPE_WEB",
    authMethodType: "OIDC_AUTH_METHOD_TYPE_BASIC",
    accessTokenType: "OIDC_TOKEN_TYPE_BEARER",
    idTokenUserinfoAssertion: true,
    devMode: true,
    loginVersion: {loginV1: {}}
}')
app=$(api POST "/management/v1/projects/$project_id/apps/_search" \
    '{"queries":[{"nameQuery":{"name":"romulus-web","method":"TEXT_QUERY_METHOD_EQUALS"}}]}' \
    | jq -c '.result[0] // empty')
client_secret=
if [ -z "$app" ]; then
    created=$(api POST "/management/v1/projects/$project_id/apps/oidc" \
        "$(jq -c '. + {name: "romulus-web"}' <<<"$oidc_config")")
    app_id=$(jq -r .appId <<<"$created")
    client_id=$(jq -r .clientId <<<"$created")
    client_secret=$(jq -r .clientSecret <<<"$created")
    log "created app romulus-web ($client_id)"
else
    app_id=$(jq -r .id <<<"$app")
    client_id=$(jq -r .oidcConfig.clientId <<<"$app")
    # COMMAND-1m88i is Zitadel's "No changes": the config is already current.
    TOLERATE=COMMAND-1m88i api PUT "/management/v1/projects/$project_id/apps/$app_id/oidc_config" "$oidc_config" >/dev/null
    if [ -f "$OUT/oidc.env" ] && grep -qx "ROMULUS_OIDC_CLIENT_ID=$client_id" "$OUT/oidc.env"; then
        client_secret=$(sed -n 's/^ROMULUS_OIDC_CLIENT_SECRET=//p' "$OUT/oidc.env")
    fi
    if [ -z "$client_secret" ]; then
        client_secret=$(api POST "/management/v1/projects/$project_id/apps/$app_id/oidc_config/_generate_client_secret" '{}' \
            | jq -r .clientSecret)
        log "regenerated the client secret of romulus-web ($client_id)"
    fi
fi

# Test users: email as username, verified, a password that never needs
# changing. The password is reset on every run so users.json is always true.
users='[]'
ensure_user() {
    local email=$1 given=$2 family=$3 user_id body
    user_id=$(api POST /v2/users \
        "$(jq -cn --arg u "$email" '{queries: [{userNameQuery: {userName: $u, method: "TEXT_QUERY_METHOD_EQUALS"}}]}')" \
        | jq -r '.result[0].userId // empty')
    if [ -z "$user_id" ]; then
        body=$(jq -cn --arg u "$email" --arg g "$given" --arg f "$family" --arg p "$DEV_PASSWORD" '{
            username: $u,
            profile: {givenName: $g, familyName: $f, displayName: ($g + " " + $f)},
            email: {email: $u, isVerified: true},
            password: {password: $p, changeRequired: false}
        }')
        user_id=$(api POST /v2/users/human "$body" | jq -r .userId)
        log "created user $email ($user_id)"
    else
        api POST "/v2/users/$user_id/password" \
            "$(jq -cn --arg p "$DEV_PASSWORD" '{newPassword: {password: $p, changeRequired: false}}')" >/dev/null
    fi
    users=$(jq -c --arg e "$email" --arg p "$DEV_PASSWORD" --arg n "$given $family" \
        '. + [{email: $e, password: $p, name: $n}]' <<<"$users")
}
ensure_user alice@romulus.test Alice Martin
ensure_user bob@romulus.test Bob Durand

# Readers must never see half a file: write beside, then rename.
mkdir -p "$OUT"
printf 'ROMULUS_OIDC_ISSUER=%s\nROMULUS_OIDC_CLIENT_ID=%s\nROMULUS_OIDC_CLIENT_SECRET=%s\n' \
    "$issuer" "$client_id" "$client_secret" > "$OUT/.oidc.env.tmp"
jq . <<<"$users" > "$OUT/.users.json.tmp"
if [ -n "$OWNER" ]; then
    chown "$OWNER" "$OUT" "$OUT/.oidc.env.tmp" "$OUT/.users.json.tmp"
fi
chmod 600 "$OUT/.oidc.env.tmp"
mv "$OUT/.oidc.env.tmp" "$OUT/oidc.env"
mv "$OUT/.users.json.tmp" "$OUT/users.json"

log "ready: issuer $issuer, client $client_id, users $(jq -r 'map(.email) | join(", ")' <<<"$users")"
