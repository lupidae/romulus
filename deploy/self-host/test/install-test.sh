#!/usr/bin/env bash
# The self-host bundle end to end: install.sh --non-interactive in a
# temporary copy of deploy/self-host and a compose project of its own, then
# install again (idempotent: same secrets), sign in, a database and a pushed
# version (SQL compiled in the sandbox, PostGIS and pgvector included),
# backup, restore, upgrade, status, admin-link, the missing-.env guard, and
# uninstall --delete-data. Whatever happens, it removes its containers,
# volumes and files.
#
#   ROMULUS_REGISTRY=local ROMULUS_VERSION=test deploy/self-host/test/install-test.sh
#       with images built beforehand: local/romulus-api:test and
#       local/romulus-sandbox:test (.github/workflows/self-host.yml); any
#       registry and version whose images are on this machine
#   ROMULUS_TEST_PLAY_DIR=/var/lib/romulus-play (with the above)
#       playgrounds on: the host set up as docs/self-host/playgrounds.md says
#       (gVisor, this directory on XFS with prjquota) and romulus-play and
#       romulus-playground on this machine too; a playground of the pushed
#       version is built, queried, deleted. On a host with no other romulus
#       install: install.sh finds playground containers by a scope every
#       install shares
#   ROMULUS_TEST_STUB=1 deploy/self-host/test/install-test.sh
#       a cheap dry run of the scripts' logic: a stub API (test/stub: health
#       and the CLI, no database) and postgres:17.11 as the sandbox; no build
#
# Needs Docker, docker compose 2.20 or later, curl, and jq but for the stub.
# ROMULUS_TEST_SUBNET picks the compose network's subnet (default: one in
# 172.30.100-249.0/24).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
bundle=$(cd "$here/.." && pwd)

step() { printf '\n== %s\n' "$*"; }
pass() { printf '   ok: %s\n' "$*"; }
fail() {
    printf 'install-test: FAIL: %s\n' "$*" >&2
    exit 1
}

stub=${ROMULUS_TEST_STUB:-}
registry=${ROMULUS_REGISTRY:-local}
version=${ROMULUS_VERSION:-test}
play_dir=${ROMULUS_TEST_PLAY_DIR:-}
images="romulus-api romulus-sandbox"
if [ -n "$play_dir" ]; then
    [ -z "$stub" ] || fail "ROMULUS_TEST_PLAY_DIR needs the real images, not the stub"
    images="$images romulus-play romulus-playground"
fi
[ -n "$stub" ] || command -v jq >/dev/null 2>&1 || fail "jq is not installed"
if [ -n "$stub" ]; then
    registry=local
    version=stub
    step "building the stub images"
    docker build -q -t local/romulus-api:stub "$here/stub" >/dev/null
    docker image inspect postgres:17.11 >/dev/null 2>&1 || docker pull -q postgres:17.11 >/dev/null
    docker tag postgres:17.11 local/romulus-sandbox:stub
fi
for image in $images; do
    docker image inspect "$registry/$image:$version" >/dev/null 2>&1 ||
        fail "the image $registry/$image:$version is not on this machine: build it first"
done
# install.sh reads these from the environment too: the options below say it.
export ROMULUS_REGISTRY=$registry ROMULUS_VERSION=$version
export ROMULUS_SUBNET=${ROMULUS_TEST_SUBNET:-172.30.$((RANDOM % 150 + 100)).0/24}

project=romulus-selftest-$$
work=$(mktemp -d "${TMPDIR:-/tmp}/romulus-selftest.XXXXXX")
cp "$bundle/compose.yaml" "$bundle/install.sh" "$bundle/backup.sh" "$bundle/Caddyfile" "$work/"

play_options=(--no-playgrounds)
[ -z "$play_dir" ] || play_options=(--playgrounds --play-dir "$play_dir")
play_id=""

port=""
for candidate in $(seq 18080 18180); do
    if ! (exec 3<>"/dev/tcp/127.0.0.1/$candidate") 2>/dev/null; then
        port=$candidate
        break
    fi
done
[ -n "$port" ] || fail "no free port in 18080-18180"
base=http://127.0.0.1:$port

dc() { docker compose --project-directory "$work" -f "$work/compose.yaml" --env-file "$work/.env" "$@"; }
sql() { dc exec -T postgres psql -X -qtA -v ON_ERROR_STOP=1 -U romulus -d "$1" -c "$2"; }

cleanup() {
    local status=$?
    if [ "$status" -ne 0 ] && [ -f "$work/.env" ]; then
        dc ps --all >&2 || true
        dc logs --tail 60 >&2 || true
    fi
    docker compose -p "$project" down --volumes --remove-orphans >/dev/null 2>&1 || true
    # The runner's own container for the playground this test made (named by
    # its id; never another install's).
    [ -z "$play_id" ] || docker rm -f "romulus-play-$play_id" >/dev/null 2>&1 || true
    docker ps -aq --filter "label=com.docker.compose.project=$project" | xargs docker rm -f >/dev/null 2>&1 || true
    docker volume ls -q --filter "label=com.docker.compose.project=$project" | xargs docker volume rm >/dev/null 2>&1 || true
    docker network ls -q --filter "label=com.docker.compose.project=$project" | xargs docker network rm >/dev/null 2>&1 || true
    rm -rf "$work"
    if [ "$status" -eq 0 ]; then
        printf '\ninstall-test: all passed (%s)\n' "$registry/{${images// /,}}:$version"
    fi
}
trap cleanup EXIT

run_install() {
    "$work/install.sh" install --non-interactive \
        --url "http://localhost:$port" --port "$port" \
        --admin-email admin@example.com --admin-name "Ada Admin" --org "Acme Test" \
        --no-caddy --majors none "${play_options[@]}" \
        --registry "$registry" --version "$version" --project "$project" --no-pull </dev/null
}

secrets() { grep -E '^(POSTGRES_PASSWORD|SANDBOX_PASSWORD|ROMULUS_PLAY_KEY|ROMULUS_PLAY_SECRET)=' "$work/.env"; }

mode_of() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }

health_ok() {
    local body
    body=$(curl -fsS --max-time 5 "$base/api/health") || fail "/api/health does not answer 200"
    [[ $body == *'"status":"ok"'* && $body == *'"database":"ok"'* ]] || fail "/api/health: $body"
    pass "/api/health: $body"
}

link_in() { grep -Eo 'https?://[^[:space:]]+#token=[^[:space:]]+' <<<"$1" | tail -n 1; }

step "install (project $project, port $port, subnet $ROMULUS_SUBNET)"
out=$(run_install 2>&1) || {
    printf '%s\n' "$out"
    fail "install.sh exited non-zero"
}
printf '%s\n' "$out"
case $(uname -m) in
x86_64 | amd64 | aarch64 | arm64)
    [[ $out != *emulated* ]] || fail "install.sh says the images run emulated on $(uname -m): they are published for it"
    pass "no emulation warning on $(uname -m)"
    ;;
esac
link=$(link_in "$out")
[ -n "$link" ] || fail "install printed no one-time link"
pass "a one-time link: $link"
[ "$(mode_of "$work/.env")" = 600 ] || fail ".env is mode $(mode_of "$work/.env"), not 600"
pass ".env is mode 600"
while IFS= read -r line; do
    [[ ${line#*=} =~ ^\'[0-9a-f]{64}\'$ ]] || fail "not a generated secret: ${line%%=*}"
done < <(secrets)
[ "$(secrets | wc -l | tr -d ' ')" = 4 ] || fail "expected 4 generated secrets"
pass "4 generated secrets"
net=${ROMULUS_SUBNET%/*}
net=${net%.*}
grep -qx "ROMULUS_PROXIES='$net.1/32'" "$work/.env" ||
    fail "a proxy on this host: only the gateway should be trusted, not $(grep '^ROMULUS_PROXIES=' "$work/.env")"
pass "trusts only the gateway (no Caddy, the port on loopback)"
health_ok
if [ -z "$stub" ]; then
    for _ in $(seq 1 60); do
        curl -fsS "$base/api/health" | grep -q '"sandbox":"ok"' && break
        sleep 2
    done
    curl -fsS "$base/api/health" | grep -q '"sandbox":"ok"' || fail "the sandbox (17) never became ok"
    pass "the sandbox compiles"
    [ "$(curl -s -o /dev/null -w '%{http_code}' "$base/app/")" = 200 ] || fail "/app/ does not answer 200"
    pass "/app/ serves the web app"
    [ "$(sql romulus 'SELECT count(*) FROM organizations')" = 1 ] || fail "setup made no organization"
    pass "setup made the organization"
    if [ -n "$play_dir" ]; then
        curl -fsS "$base/api/health" | grep -q '"playgrounds":"ok"' ||
            fail "the playground host is not ok: $(curl -fsS "$base/api/health")"
        pass "the playground host answers"
    fi
fi

step "install again: idempotent"
before=$(secrets)
out=$(run_install 2>&1) || {
    printf '%s\n' "$out"
    fail "the second install exited non-zero"
}
printf '%s\n' "$out" | tail -n 12
[ "$(secrets)" = "$before" ] || fail "the second install changed a secret"
pass "same secrets"
if [ -z "$stub" ]; then
    [ -n "$(link_in "$out")" ] || fail "setup again printed no fresh link (the password was never set)"
    pass "a fresh link"
    [ "$(sql romulus 'SELECT count(*) FROM organizations')" = 1 ] || fail "the second install made another organization"
    pass "still one organization"

    step "the admin's link sets a password; sign in with it"
    link=$(link_in "$out")
    token=${link##*#token=}
    jar=$work/cookies
    post() {
        curl -s -o /dev/null -w '%{http_code}' -b "$jar" -c "$jar" \
            -H "Origin: http://localhost:$port" -H 'X-Romulus-Csrf: 1' \
            -H 'Content-Type: application/json' --data "$2" "$base$1"
    }
    status=$(post /api/auth/password/reset "{\"token\":\"$token\",\"password\":\"violet tractor mango\"}")
    [ "$status" = 204 ] || fail "setting the password from the link answered $status"
    curl -fsS -b "$jar" "$base/api/orgs" | grep -q '"slug":"acme-test"' || fail "the admin does not see the organization"
    pass "the link set the password and signed the admin in"
    status=$(post /api/auth/password/sign-in '{"email":"admin@example.com","password":"wrong password here"}')
    [ "$status" = 401 ] || fail "a wrong password answered $status"
    status=$(post /api/auth/password/sign-in '{"email":"ADMIN@example.com","password":"violet tractor mango"}')
    [ "$status" = 204 ] || fail "password sign-in answered $status"
    pass "password sign-in (and a wrong password refused)"
    status=$(post /api/orgs '{"name":"Second","slug":"second"}')
    [ "$status" = 403 ] || fail "a second organization answered $status, not 403 (single organization)"
    pass "one organization only"
    [ "$(curl -s -o /dev/null -w '%{redirect_url}' "$base/")" = "$base/app/" ] || fail "/ does not lead to /app/"
    pass "/ leads to the app"

    # api METHOD PATH [JSON]: as the signed-in admin; prints the status, the
    # body is in $work/body.
    api() {
        local args=(-s -o "$work/body" -w '%{http_code}' -b "$jar" -c "$jar" -X "$1"
            -H "Origin: http://localhost:$port" -H 'X-Romulus-Csrf: 1')
        [ $# -lt 3 ] || args+=(-H 'Content-Type: application/json' --data "$3")
        curl "${args[@]}" "$base$2"
    }
    body() { cat "$work/body"; }
    # expect STATUS JQ_FILTER WHAT: the last call answered STATUS with a body
    # satisfying the filter.
    expect() {
        if [ "$status" != "$1" ] || ! body | jq -e "$2" >/dev/null; then
            fail "$3: $status $(body)"
        fi
    }

    step "a database, a token, a pushed version"
    status=$(api POST /api/orgs/acme-test/databases '{"name":"Shop","slug":"shop"}')
    expect 201 '.slug == "shop"' "creating a database"
    shop=$(body | jq -r .id)
    status=$(api POST /api/orgs/acme-test/databases/shop/tokens '{"name":"ci","scope":"push"}')
    expect 201 '.secret | startswith("rom_")' "a push token"
    token=$(body | jq -r .secret)
    # PostGIS and pgvector: the sandbox's extensions, built for this host.
    schema='CREATE EXTENSION postgis;
CREATE EXTENSION vector;
CREATE TABLE customers (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email text NOT NULL UNIQUE
);
CREATE TABLE orders (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id bigint NOT NULL REFERENCES customers (id),
    total numeric(12, 2) NOT NULL CHECK (total >= 0)
);
CREATE INDEX orders_customer_id_idx ON orders (customer_id);
CREATE TABLE stores (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    location geometry(Point, 4326) NOT NULL,
    embedding vector(3)
);'
    push() {
        curl -s -o "$work/body" -w '%{http_code}' -H "Authorization: Bearer $token" \
            -H 'Content-Type: application/sql' --data-binary "$schema" "$base/api/databases/$shop/versions?message=$1"
    }
    status=$(push first)
    expect 201 '.status == "created" and .version.number == 1' "the push"
    status=$(push again)
    expect 200 '.status == "unchanged" and .version.number == 1' "the same push again"
    status=$(api GET /api/orgs/acme-test/databases/shop/versions)
    expect 200 '.versions | length == 1' "shop's versions"
    pass "version 1, compiled in the sandbox (PostGIS, pgvector); the same push again changes nothing"

    if [ -n "$play_dir" ]; then
        step "a playground of version 1 (gVisor, $play_dir)"
        status=$(api POST /api/orgs/acme-test/databases/shop/playgrounds '{"version":1,"seed":"5eed"}')
        expect 201 '.status == "building"' "creating a playground"
        play_id=$(body | jq -r .id)
        for _ in $(seq 1 150); do
            status=$(api GET "/api/orgs/acme-test/databases/shop/playgrounds/$play_id")
            [ "$status" = 200 ] || fail "reading the playground answered $status: $(body)"
            [ "$(body | jq -r .status)" = building ] || break
            sleep 2
        done
        expect 200 '.status == "ready" and ([.tables[].rows] | add) > 0' "the playground's build"
        pass "ready: $(body | jq -r '[.tables[] | "\(.table) \(.rows)"] | join(", ")') rows"
        status=$(api POST "/api/orgs/acme-test/databases/shop/playgrounds/$play_id/query" \
            '{"sql":"SELECT (SELECT count(*) FROM orders) AS orders, (SELECT bool_and(ST_IsValid(location)) FROM stores) AS valid, (SELECT count(embedding <-> '"'"'[1,2,3]'"'"') FROM stores) AS near"}')
        expect 200 '.status == "ok" and (.statements[0].rows | length) == 1' "a query in the console"
        pass "queried as play_read: $(body | jq -c '.statements[0].rows[0]')"
        status=$(api POST "/api/orgs/acme-test/databases/shop/playgrounds/$play_id/query" '{"sql":"DELETE FROM orders"}')
        # A read-only transaction (25006), and play_read's privileges (42501) behind it.
        expect 200 '.status == "error" and (.statements[-1].error.sqlstate | . == "25006" or . == "42501")' \
            "a write in the console"
        pass "a write in the console is refused: $(body | jq -r '.statements[-1].error.message')"
        status=$(api DELETE "/api/orgs/acme-test/databases/shop/playgrounds/$play_id")
        [ "$status" = 204 ] || fail "deleting the playground answered $status: $(body)"
        for _ in $(seq 1 30); do
            docker inspect "romulus-play-$play_id" >/dev/null 2>&1 || break
            sleep 1
        done
        ! docker inspect "romulus-play-$play_id" >/dev/null 2>&1 || fail "the playground's container is still there"
        pass "deleted, its container gone"
    fi
fi
health_ok

step "backup"
sql romulus "CREATE TABLE install_test_marker (v text); INSERT INTO install_test_marker VALUES ('before-backup')" >/dev/null
"$work/install.sh" backup "$work/test-backups"
dump=$(ls "$work"/test-backups/romulus-*-manual.dump 2>/dev/null) || fail "backup wrote no dump"
[ "$(printf '%s\n' "$dump" | wc -l | tr -d ' ')" = 1 ] || fail "expected one dump, got: $dump"
[ -f "${dump%.dump}.env" ] || fail "no copy of .env beside the dump"
[ "$(mode_of "${dump%.dump}.env")" = 600 ] || fail "the .env copy is not mode 600"
dc exec -T postgres pg_restore --list <"$dump" >/dev/null || fail "the dump does not read back"
pass "$(basename "$dump") and its .env"
ls "$work/test-backups"/*.partial >/dev/null 2>&1 && fail "a partial dump was left behind"

step "restore"
sql romulus "INSERT INTO install_test_marker VALUES ('after-backup')" >/dev/null
"$work/install.sh" restore "$dump" </dev/null
[ "$(sql romulus "SELECT string_agg(v, ',' ORDER BY v) FROM install_test_marker")" = before-backup ] ||
    fail "the restored database is not the dump's"
pass "the database is the dump's"
[ "$(sql romulus_before_restore "SELECT string_agg(v, ',' ORDER BY v) FROM install_test_marker")" = after-backup,before-backup ] ||
    fail "the replaced database was not kept as romulus_before_restore"
pass "the replaced one is romulus_before_restore"
ls "$work"/backups/romulus-*-pre-restore.dump >/dev/null 2>&1 || fail "no pre-restore dump"
pass "a pre-restore dump"
if [ -z "$stub" ]; then
    [ "$(sql romulus 'SELECT count(*) FROM organizations')" = 1 ] || fail "the organization did not survive the restore"
    pass "the organization is back"
fi
health_ok

step "upgrade to the same version"
"$work/install.sh" upgrade "$version" --no-pull </dev/null
ls "$work"/backups/romulus-*-pre-upgrade-"$version"-to-"$version".dump >/dev/null 2>&1 || fail "no pre-upgrade dump"
pass "a pre-upgrade dump"
grep -q "^ROMULUS_PREVIOUS_VERSION='$version'$" "$work/.env" || fail "the previous version is not recorded"
[ "$(sql romulus "SELECT string_agg(v, ',' ORDER BY v) FROM install_test_marker")" = before-backup ] ||
    fail "the upgrade changed the data"
health_ok

step "status and admin-link"
"$work/install.sh" status >/dev/null || fail "status exited non-zero"
pass "status"
out=$("$work/install.sh" admin-link admin@example.com </dev/null) || fail "admin-link exited non-zero"
[ -n "$(link_in "$out")" ] || fail "admin-link printed no link: $out"
pass "admin-link: $(link_in "$out")"

step "install without .env but with the data: refused"
mv "$work/.env" "$work/.env.saved"
if out=$(run_install 2>&1); then
    mv "$work/.env.saved" "$work/.env"
    fail "install went ahead with new passwords over existing data"
fi
mv "$work/.env.saved" "$work/.env"
[[ $out == *".env is gone"* ]] || fail "refused, but not for the missing .env: $out"
pass "refused: ${out##*install.sh: }"

step "uninstall --delete-data"
"$work/install.sh" uninstall --delete-data --confirm "$project" </dev/null
[ ! -f "$work/.env" ] || fail ".env is still there"
[ -z "$(docker volume ls -q --filter "label=com.docker.compose.project=$project")" ] || fail "volumes are left"
[ -z "$(docker ps -aq --filter "label=com.docker.compose.project=$project")" ] || fail "containers are left"
pass "no containers, no volumes, no .env"
