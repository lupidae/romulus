#!/usr/bin/env bash
# Makes the dev Zitadel instance usable by romulus, with no clicking:
#   project "romulus" → OIDC web app "romulus-web" → test users alice and bob,
#   the hosted login branded as romulus (logo, colors, font, texts), and its
#   emails (sign-up codes) sent to the stack's mail catcher.
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
ASSETS=${ASSETS:-/assets}       # deploy/zitadel/assets: logos, font, brand.json
MAIL_HOST=${MAIL_HOST:-}        # host:port of an SMTP server without auth; empty: no email

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

# The hosted login looks like romulus: Zitadel's label policy (colors from the
# app's tokens via brand.json, the logo per theme, Inter, no watermark) on the
# instance default, which every organization inherits, and the project tells
# the login to use its own organization's policy. `npm --prefix web run
# zitadel:assets` regenerates the assets after a brand change.
brand=$(cat "$ASSETS/brand.json")
label_policy=$(jq -c '{
    primaryColor: .light.primary, backgroundColor: .light.background,
    warnColor: .light.destructive, fontColor: .light.foreground,
    primaryColorDark: .dark.primary, backgroundColorDark: .dark.background,
    warnColorDark: .dark.destructive, fontColorDark: .dark.foreground,
    hideLoginNameSuffix: true, disableWatermark: true, themeMode: "THEME_MODE_AUTO"
}' <<<"$brand")
# INSTANCE-28fHe is "has not been changed".
TOLERATE=INSTANCE-28fHe api PUT /admin/v1/policies/label "$label_policy" >/dev/null
# upload FILE ASSET: one image or font into the (preview) label policy.
upload() {
    local response status
    response=$(curl -sS -X POST "$ZITADEL_URL/assets/v1/instance/policy/label/$2" \
        -H "Host: $ZITADEL_HOST" -H "Authorization: Bearer $pat" \
        -F "file=@$ASSETS/$1" -w '\n%{http_code}')
    status=${response##*$'\n'}
    [ "${status:0:1}" = 2 ] || { log "upload $1 → $status: ${response%$'\n'*}"; return 1; }
}
upload logo-light.png logo
upload logo-dark.png logo/dark
upload icon-light.png icon
upload icon-dark.png icon/dark
upload inter.woff2 font
TOLERATE=COMMAND-1m88i api POST /admin/v1/policies/label/_activate '{}' >/dev/null
TOLERATE=COMMAND-1m88i api PUT "/management/v1/projects/$project_id" \
    '{"name":"romulus","privateLabelingSetting":"PRIVATE_LABELING_SETTING_ENFORCE_PROJECT_RESOURCE_OWNER_POLICY"}' >/dev/null

# The app speaks English only, so does its login (other languages would show
# Zitadel's own texts). The texts say what romulus says: sentence case, plain.
TOLERATE=COMMAND-1m88i api PUT /admin/v1/restrictions '{"allowedLanguages":{"list":["en"]}}' >/dev/null
api PUT /admin/v1/text/login/en "$(jq -cn '{
    loginText: {
        title: "Sign in to romulus",
        description: "Use your work email.",
        loginNameLabel: "Email",
        loginNamePlaceholder: "name@company.com",
        registerButtonText: "Create account",
        nextButtonText: "Continue"
    },
    passwordText: {
        title: "Enter your password",
        description: "Signing in to romulus.",
        resetLinkText: "Forgot your password?",
        nextButtonText: "Sign in"
    },
    selectAccountText: {
        title: "Choose an account",
        description: "Which account should romulus use?",
        otherUser: "Use another account"
    },
    registrationUserText: {
        title: "Create your romulus account",
        description: "Invited? Use the email your invitation was sent to.",
        firstnameLabel: "First name",
        lastnameLabel: "Last name",
        emailLabel: "Work email",
        passwordConfirmLabel: "Confirm password",
        nextButtonText: "Create account",
        backButtonText: "Back to sign-in"
    },
    initializeUserText: {
        title: "Check your email",
        description: "We sent you a code. Enter it to verify your address and finish creating your account.",
        resendButtonText: "Send a new code",
        nextButtonText: "Verify"
    },
    initializeDoneText: {
        title: "Your account is ready",
        description: "Your email is verified.",
        nextButtonText: "Continue to romulus"
    },
    emailVerificationText: {
        title: "Check your email",
        description: "We sent you a code. Enter it to verify your address.",
        resendButtonText: "Send a new code",
        nextButtonText: "Verify"
    },
    emailVerificationDoneText: {
        title: "Email verified",
        description: "You can continue to romulus.",
        nextButtonText: "Continue to romulus"
    },
    logoutText: {
        title: "Signed out",
        description: "You signed out of romulus.",
        loginButtonText: "Sign in again"
    }
}')" >/dev/null
api PUT /admin/v1/text/message/init/en "$(jq -cn '{
    title: "Verify your email",
    preHeader: "Your romulus verification code",
    subject: "Your romulus verification code: {{.Code}}",
    greeting: "Hello {{.DisplayName}},",
    text: "Your code is {{.Code}}. Enter it on the page that asked for it to finish creating your romulus account. If you did not ask for this, ignore this email.",
    buttonText: "Verify my email"
}')" >/dev/null
log "login branded as romulus"

# Sign-up and password-reset codes go by email. In dev the stack's mail
# catcher receives them (its web UI: http://localhost:$ROMULUS_MAIL_PORT).
if [ -n "$MAIL_HOST" ]; then
    smtp_id=$(api POST /admin/v1/email/_search '{}' \
        | jq -r '.result[]? | select(.description == "romulus dev mail") | .id' | head -1)
    if [ -z "$smtp_id" ]; then
        smtp_id=$(api POST /admin/v1/email/smtp "$(jq -cn --arg host "$MAIL_HOST" '{
            description: "romulus dev mail", host: $host, tls: false,
            senderAddress: "no-reply@romulus.localhost", senderName: "romulus"
        }')" | jq -r .id)
        log "added SMTP $MAIL_HOST ($smtp_id)"
    fi
    # COMMAND-vUHBSmBzaw is "already active": nothing to do.
    TOLERATE=COMMAND-vUHBSmBzaw api POST "/admin/v1/email/$smtp_id/_activate" '{}' >/dev/null
fi

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
