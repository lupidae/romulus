#!/usr/bin/env bash
# The chart end to end on a throwaway k3d cluster (k3s: its own network
# policy controller enforces the chart's NetworkPolicies), in a namespace
# enforcing Pod Security "restricted":
#
#   install, wait, /api/health (database, sandboxes 17 and 18), the first
#   admin (`romulus-api setup` through kubectl exec, its link sets the
#   password), a database on 17 and one on 18, a push (SQL compiled in each
#   sandbox), the NetworkPolicies (a stranger pod reaches neither Postgres
#   nor a sandbox; the backup job does), the daily dump; `helm upgrade` with
#   changed values (a pre-upgrade dump, the setup Job; secrets, data and
#   sign-in survive), restore.sh, `helm test`; uninstall (the volumes and
#   the Secret stay), install again on them, and the guard refusing new
#   secrets over old volumes. Whatever happens, it deletes its cluster.
#
#   deploy/helm/test/e2e.sh
#
# Environment:
#   ROMULUS_IMAGE_TAG    the images' tag (default: the sha of origin/main,
#                        published by .github/workflows/images.yml)
#   ROMULUS_REGISTRY     default ghcr.io/lupidae
#   ROMULUS_E2E_CLUSTER  the k3d cluster (default romulus-helm-e2e)
#   ROMULUS_E2E_K3S      the k3s image (default rancher/k3s:v1.36.4-k3s1)
#   ROMULUS_E2E_KEEP=1   keep the cluster afterwards, to look around
#
# The images are published for amd64 and arm64 (multi-arch lists): a node
# pulls its own. Tags published before arm64 builds existed are amd64 only:
# on an arm64 host (an Apple silicon Mac's), each of those is relabelled for
# the host's platform (the same layers, run by its amd64 emulation, e.g.
# OrbStack's Rosetta) into a tarball and imported into the cluster, under
# the same name.
#
# Needs docker, k3d 5, kubectl, helm (3.14 or later, or 4), curl, jq. The
# kubeconfig is the script's own: the default one is never touched.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
chart=$(cd "$here/../romulus" && pwd)
root=$(cd "$here/../../.." && pwd)

step() { printf '\n== %s\n' "$*"; }
pass() { printf '   ok: %s\n' "$*"; }
fail() {
    printf 'e2e: FAIL: %s\n' "$*" >&2
    exit 1
}

for tool in docker k3d kubectl helm curl jq; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is not installed"
done

registry=${ROMULUS_REGISTRY:-ghcr.io/lupidae}
tag=${ROMULUS_IMAGE_TAG:-$(git -C "$root" rev-parse origin/main)}
cluster=${ROMULUS_E2E_CLUSTER:-romulus-helm-e2e}
release=romulus
ns=romulus-e2e
k3s=${ROMULUS_E2E_K3S:-rancher/k3s:v1.36.4-k3s1}
postgres_image=$(sed -n '/^postgresql:/,/^[a-z]/{s/^    repository: //p;s/^    tag: "\(.*\)"$/\1/p;}' "$chart/values.yaml" | paste -sd: -)

work=$(mktemp -d "${TMPDIR:-/tmp}/romulus-helm-e2e.XXXXXX")
export KUBECONFIG=$work/kubeconfig
forward_pid=""

cleanup() {
    local status=$?
    [ -z "$forward_pid" ] || kill "$forward_pid" 2>/dev/null || true
    if [ "$status" -ne 0 ] && [ -f "$KUBECONFIG" ]; then
        kubectl -n "$ns" get pods,pvc,jobs -o wide >&2 || true
        kubectl -n "$ns" get events --sort-by=.lastTimestamp 2>/dev/null | tail -n 30 >&2 || true
        kubectl -n "$ns" logs "deploy/$release-api" --all-containers --tail 60 >&2 || true
    fi
    if [ -n "${ROMULUS_E2E_KEEP:-}" ]; then
        printf '\nkept the cluster %s: KUBECONFIG=%s; k3d cluster delete %s\n' "$cluster" "$KUBECONFIG" "$cluster"
    else
        k3d cluster delete "$cluster" >/dev/null 2>&1 || true
        rm -rf "$work"
    fi
    [ "$status" -eq 0 ] || printf '\ne2e: FAILED (exit %s)\n' "$status" >&2
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

port=""
for candidate in $(seq 18280 18380); do
    if ! (exec 3<>"/dev/tcp/127.0.0.1/$candidate") 2>/dev/null; then
        port=$candidate
        break
    fi
done
[ -n "$port" ] || fail "no free port in 18280-18380"
origin=http://localhost:$port
base=http://127.0.0.1:$port

k() { kubectl -n "$ns" "$@"; }
psql_app() { k exec "$release-postgresql-0" -c postgres -- psql -X -qtA -v ON_ERROR_STOP=1 -U romulus -d "$1" -c "$2"; }
secret_data() { k get secret "$release" -o jsonpath='{.data}'; }

# The values every install here uses: small enough for a laptop's Docker.
cat >"$work/values.yaml" <<EOF
host: romulus.test
publicUrl: $origin
image:
  registry: $registry
  tag: "$tag"
  pullPolicy: IfNotPresent
sandbox:
  extraMajors: [18]
  resources:
    requests: {cpu: 500m, memory: 512Mi}
    limits: {cpu: 500m, memory: 512Mi}
  persistence: {size: 1Gi}
api:
  resources:
    requests: {cpu: 50m, memory: 128Mi}
    limits: {memory: 512Mi}
postgresql:
  persistence: {size: 1Gi}
  resources:
    requests: {cpu: 50m, memory: 128Mi}
    limits: {memory: 512Mi}
backup:
  persistence: {size: 1Gi}
EOF

install() {
    helm install "$release" "$chart" -n "$ns" "${values[@]}" --wait --timeout 15m
}

forward() {
    [ -z "$forward_pid" ] || kill "$forward_pid" 2>/dev/null || true
    k rollout status "deploy/$release-api" --timeout 10m >/dev/null
    kubectl -n "$ns" port-forward "svc/$release-api" "$port:8080" >"$work/forward.log" 2>&1 &
    forward_pid=$!
    for _ in $(seq 1 30); do
        curl -fsS --max-time 3 "$base/api/health" >/dev/null 2>&1 && return 0
        sleep 1
    done
    cat "$work/forward.log" >&2
    fail "the port-forward never answered"
}

health() { curl -fsS --max-time 5 "$base/api/health"; }

# wait_health JQ_FILTER SECONDS: until /api/health satisfies the filter.
wait_health() {
    local body=""
    for _ in $(seq 1 "$2"); do
        body=$(health 2>/dev/null || true)
        if [ -n "$body" ] && jq -e "$1" >/dev/null 2>&1 <<<"$body"; then
            pass "/api/health: $body"
            return 0
        fi
        sleep 1
    done
    fail "/api/health never satisfied $1: $body"
}

jar=$work/cookies
# api METHOD PATH [JSON]: the status on the first line, the body after.
api() {
    local args=(-s -o "$work/body" -w '%{http_code}' -b "$jar" -c "$jar" -X "$1")
    if [ $# -ge 3 ]; then
        args+=(-H "Origin: $origin" -H 'X-Romulus-Csrf: 1' -H 'Content-Type: application/json' --data "$3")
    elif [ "$1" != GET ]; then
        args+=(-H "Origin: $origin" -H 'X-Romulus-Csrf: 1')
    fi
    curl "${args[@]}" "$base$2"
}
body() { cat "$work/body"; }

# expect STATUS JQ_FILTER WHAT: the last api call answered STATUS with a body
# satisfying the filter.
expect() {
    if [ "$status" != "$1" ] || ! body | jq -e "$2" >/dev/null; then
        fail "$3: $status $(body)"
    fi
}

# A pod of its own (Pod Security "restricted") running the Postgres image's
# COMMAND; prints its exit code.
probe() {
    local name=$1 command
    shift
    command=$(printf '"%s", ' "$@")
    k delete pod "$name" --ignore-not-found --wait >/dev/null
    k apply -f - >/dev/null <<EOF || return 0
apiVersion: v1
kind: Pod
metadata:
  name: $name
spec:
  restartPolicy: Never
  automountServiceAccountToken: false
  securityContext:
    runAsNonRoot: true
    runAsUser: 999
    seccompProfile: {type: RuntimeDefault}
  containers:
    - name: probe
      image: $postgres_image
      command: [${command%, }]
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        capabilities: {drop: [ALL]}
EOF
    for _ in $(seq 1 120); do
        case $(k get pod "$name" -o jsonpath='{.status.phase}' 2>/dev/null) in
        Succeeded | Failed | "") break ;;
        esac
        sleep 1
    done
    k get pod "$name" -o jsonpath='{.status.containerStatuses[0].state.terminated.exitCode}'
    k delete pod "$name" --wait=false >/dev/null
}

step "a k3d cluster ($cluster), the images $registry/romulus-*:$tag"
k3d cluster delete "$cluster" >/dev/null 2>&1 || true
k3d cluster create "$cluster" --image "$k3s" --no-lb --wait --timeout 300s \
    --kubeconfig-update-default=false --kubeconfig-switch-context=false \
    --k3s-arg '--disable=traefik@server:0' --k3s-arg '--disable=servicelb@server:0' \
    --k3s-arg '--disable=metrics-server@server:0' >/dev/null
k3d kubeconfig get "$cluster" >"$KUBECONFIG"
kubectl wait --for=condition=Ready node --all --timeout 120s >/dev/null
pass "cluster up: $(kubectl version -o json 2>/dev/null | jq -r .serverVersion.gitVersion)"
case $(docker info --format '{{.Architecture}}') in
x86_64 | amd64) ;;
aarch64 | arm64)
    relabelled=""
    for image in "$registry/romulus-api:$tag" "$registry/romulus-sandbox:$tag" "$registry/romulus-sandbox:$tag-pg18"; do
        docker buildx imagetools inspect --raw "$image" |
            jq -e '[.manifests[]?.platform | select(.os == "linux" and .architecture == "arm64")] | length > 0' >/dev/null &&
            continue
        mkdir -p "$work/relabel"
        tarball=$work/relabel/$(basename "${image//:/-}").tar
        (cd "$work/relabel" && docker buildx build -q --platform linux/arm64 --provenance=false \
            --output "type=docker,dest=$tarball" -t "$image" - <<<"FROM --platform=linux/amd64 $image") >/dev/null
        k3d image import -c "$cluster" "$tarball" >/dev/null
        rm -f "$tarball"
        relabelled="$relabelled ${image##*/}"
    done
    if [ -n "$relabelled" ]; then
        # Rosetta maps more than the sandbox's 128 MB per-process cap allows
        # before Postgres even starts: the emulated run raises it.
        printf 'sandbox:\n  maxDataKb: 1048576\n' >"$work/emulated.yaml"
        pass "amd64 only, relabelled for arm64 (emulated) and imported:$relabelled"
    else
        pass "published for arm64: the cluster pulls them"
    fi
    ;;
*) fail "the images run on amd64 and arm64; not on $(docker info --format '{{.Architecture}}')" ;;
esac
values=(-f "$work/values.yaml")
[ ! -f "$work/emulated.yaml" ] || values+=(-f "$work/emulated.yaml")
kubectl create namespace "$ns" >/dev/null
kubectl label namespace "$ns" pod-security.kubernetes.io/enforce=restricted \
    pod-security.kubernetes.io/enforce-version=latest >/dev/null
pass "namespace $ns, Pod Security restricted enforced"

step "helm install"
install
pass "installed: $(k get pods --no-headers | awk '{print $1 " " $3}' | tr '\n' ' ')"
forward
wait_health '.status == "ok" and .database == "ok" and .playgrounds == "off"' 30
wait_health '.sandbox == "ok" and .sandboxes["18"] == "ok" and .sandboxes["15"] == "off"' 240
[ "$(curl -s -o /dev/null -w '%{http_code}' "$base/app/")" = 200 ] || fail "/app/ does not answer 200"
pass "/app/ serves the web app"

step "the first admin, as NOTES.txt says"
out=$(k exec "deploy/$release-api" -- romulus-api setup --email admin@example.com --name "Ada Admin" --org "Acme Test")
link=$(grep -Eo 'https?://[^[:space:]]+#token=[^[:space:]]+' <<<"$out" | tail -n 1)
[ -n "$link" ] || fail "setup printed no link: $out"
[[ $link == "$origin/"* ]] || fail "the link is not on the public URL: $link"
pass "a one-time link: $link"
password="violet tractor mango"
status=$(api POST /api/auth/password/reset "{\"token\":\"${link##*#token=}\",\"password\":\"$password\"}")
[ "$status" = 204 ] || fail "setting the password from the link answered $status: $(body)"
status=$(api GET /api/orgs)
expect 200 'map(.slug) == ["acme-test"]' "the admin's organizations"
pass "the link set the password, the admin is in acme-test"
status=$(api POST /api/orgs '{"name":"Second","slug":"second"}')
[ "$status" = 403 ] || fail "a second organization answered $status, not 403 (single organization)"
pass "one organization only"

step "databases, a token, pushes compiled in each sandbox"
status=$(api POST /api/orgs/acme-test/databases '{"name":"Shop","slug":"shop"}')
[ "$status" = 201 ] || fail "creating a database answered $status: $(body)"
shop=$(body | jq -r .id)
status=$(api POST /api/orgs/acme-test/databases '{"name":"Legacy","slug":"legacy","pg_major":18}')
[ "$status" = 201 ] || fail "creating a database on 18 answered $status: $(body)"
legacy=$(body | jq -r .id)
pass "shop (17) and legacy (18)"
push() {
    local database=$1 token
    status=$(api POST "/api/orgs/acme-test/databases/$2/tokens" '{"name":"ci","scope":"push"}')
    [ "$status" = 201 ] || fail "a token for $2 answered $status: $(body)"
    token=$(body | jq -r .secret)
    status=$(curl -s -o "$work/body" -w '%{http_code}' -H "Authorization: Bearer $token" \
        -H 'Content-Type: application/sql' --data-binary "$3" "$base/api/databases/$database/versions?message=e2e")
    [ "$status" = 201 ] || fail "pushing to $2 answered $status: $(body)"
    body | jq -e '.status == "created" and .version.number == 1' >/dev/null || fail "push to $2: $(body)"
}
push "$shop" shop 'CREATE TABLE customers (id bigint PRIMARY KEY, email text NOT NULL UNIQUE);
CREATE TABLE orders (id bigint PRIMARY KEY, customer_id bigint NOT NULL REFERENCES customers (id), total numeric(12, 2));'
push "$legacy" legacy 'CREATE TABLE events (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, payload jsonb NOT NULL);'
status=$(api GET /api/orgs/acme-test/databases/shop/versions)
expect 200 '.versions | length == 1' "shop's versions"
pass "version 1 of each, compiled on 17 and on 18"

step "NetworkPolicies"
code=$(probe stranger-to-postgres pg_isready -h "$release-postgresql" -p 5432 -t 5)
[ "$code" = 2 ] || fail "a stranger pod reached Postgres (pg_isready exit $code, not 2: no response)"
pass "a stranger pod does not reach Postgres"
code=$(probe stranger-to-sandbox pg_isready -h "$release-sandbox" -p 5432 -t 5)
[ "$code" = 2 ] || fail "a stranger pod reached the sandbox (pg_isready exit $code)"
pass "a stranger pod does not reach the sandbox"
k create job --from="cronjob/$release-backup" e2e-backup >/dev/null
k wait --for=condition=complete job/e2e-backup --timeout 300s >/dev/null || fail "the backup job did not complete: $(k logs job/e2e-backup)"
daily=$(k exec "$release-postgresql-0" -c postgres -- sh -c 'ls /backups/romulus-*-daily.dump')
[ -n "$daily" ] || fail "the backup job wrote no daily dump"
pass "the backup job reaches Postgres: $daily"

step "helm upgrade with changed values"
before_secrets=$(secret_data)
before_pod=$(k get pods -l app.kubernetes.io/component=api -o jsonpath='{.items[0].metadata.name}')
psql_app romulus "CREATE TABLE e2e_marker (v text); INSERT INTO e2e_marker VALUES ('before-upgrade')" >/dev/null
helm upgrade "$release" "$chart" -n "$ns" "${values[@]}" --wait --timeout 15m \
    --set api.logLevel='romulus_api=debug\,tower_http=info\,sqlx=warn' \
    --set admin.setupJob=true --set admin.email=admin@example.com \
    --set admin.name="Ada Admin" --set admin.organization="Acme Test" >/dev/null
pre_upgrade=$(k exec "$release-postgresql-0" -c postgres -- sh -c "ls /backups/romulus-*-pre-upgrade-$tag-to-$tag.dump")
[ -n "$pre_upgrade" ] || fail "no pre-upgrade dump"
pass "a pre-upgrade dump: $pre_upgrade"
[ "$(secret_data)" = "$before_secrets" ] || fail "the upgrade changed the Secret"
pass "the same secrets"
after_pod=$(k get pods -l app.kubernetes.io/component=api -o jsonpath='{.items[0].metadata.name}')
[ "$after_pod" != "$before_pod" ] || fail "the API was not restarted with its new settings"
k get deploy "$release-api" -o json | jq -e '.spec.template.spec.containers[0].env[] | select(.name == "RUST_LOG") | .value == "romulus_api=debug,tower_http=info,sqlx=warn"' >/dev/null ||
    fail "the API's RUST_LOG did not change"
pass "the API restarted with the new log level"
k logs "job/$release-setup" | grep -q 'already set up' || fail "the setup Job: $(k logs "job/$release-setup")"
pass "the setup Job: already set up, nothing printed"
[ "$(psql_app romulus 'SELECT v FROM e2e_marker')" = before-upgrade ] || fail "the data did not survive"
forward
wait_health '.database == "ok" and .sandbox == "ok"' 120
rm -f "$jar"
status=$(api POST /api/auth/password/sign-in "{\"email\":\"admin@example.com\",\"password\":\"$password\"}")
[ "$status" = 204 ] || fail "signing in after the upgrade answered $status: $(body)"
status=$(api GET /api/orgs/acme-test/databases/shop/versions)
expect 200 '.versions | length == 1' "shop's versions after the upgrade"
pass "signed in with the same password; the versions are there"

step "restore.sh"
psql_app romulus "INSERT INTO e2e_marker VALUES ('after-upgrade')" >/dev/null
if out=$(k exec "$release-postgresql-0" -c postgres -- bash /scripts/restore.sh "$pre_upgrade" 2>&1); then
    fail "restore.sh went ahead with the API connected"
fi
[[ $out == *"stop the API first"* ]] || fail "restore.sh refused, but not for the API: $out"
pass "refused while the API runs"
k scale "deploy/$release-api" --replicas=0 >/dev/null
k wait --for=delete pod -l app.kubernetes.io/component=api --timeout 120s >/dev/null 2>&1 || true
k exec "$release-postgresql-0" -c postgres -- bash /scripts/restore.sh "$pre_upgrade" ||
    fail "restore.sh failed"
[ "$(psql_app romulus "SELECT string_agg(v, ',' ORDER BY v) FROM e2e_marker")" = before-upgrade ] ||
    fail "the restored database is not the dump's"
[ "$(psql_app romulus_before_restore "SELECT string_agg(v, ',' ORDER BY v) FROM e2e_marker")" = after-upgrade,before-upgrade ] ||
    fail "the replaced database was not kept as romulus_before_restore"
k exec "$release-postgresql-0" -c postgres -- sh -c 'ls /backups/romulus-*-pre-restore.dump' >/dev/null ||
    fail "no pre-restore dump"
pass "restored; the replaced database is romulus_before_restore; a pre-restore dump"
k scale "deploy/$release-api" --replicas=1 >/dev/null
forward
wait_health '.database == "ok"' 60
helm test "$release" -n "$ns" --timeout 5m >/dev/null || fail "helm test failed"
pass "helm test"

step "uninstall keeps the data; install again on it"
claims="data-$release-postgresql-0 data-$release-sandbox-0 data-$release-sandbox-18-0 $release-backups"
helm uninstall "$release" -n "$ns" --wait --timeout 5m >/dev/null
for claim in $claims; do
    k get pvc "$claim" >/dev/null 2>&1 || fail "uninstall deleted the volume $claim"
done
[ "$(secret_data)" = "$before_secrets" ] || fail "uninstall deleted or changed the Secret"
pass "kept: $claims, the Secret $release"
install >/dev/null
[ "$(secret_data)" = "$before_secrets" ] || fail "the new install replaced the Secret"
forward
wait_health '.database == "ok" and .sandbox == "ok"' 120
rm -f "$jar"
status=$(api POST /api/auth/password/sign-in "{\"email\":\"admin@example.com\",\"password\":\"$password\"}")
[ "$status" = 204 ] || fail "signing in on the reinstalled release answered $status: $(body)"
status=$(api GET /api/orgs/acme-test/databases/shop/versions)
expect 200 '.versions | length == 1' "shop's versions on the reinstalled release"
pass "the same Secret, the same data, the same password"

step "the Secret gone but not the volumes: refused"
helm uninstall "$release" -n "$ns" --wait --timeout 5m >/dev/null
k delete secret "$release" >/dev/null
if out=$(install 2>&1); then
    fail "installed new passwords over the existing volumes"
fi
[[ $out == *"exists but the Secret $release is gone"* ]] || fail "refused, but not for the Secret: $out"
pass "refused: ${out##*execution error at }"

printf '\ne2e: all passed (%s/romulus-*:%s)\n' "$registry" "$tag"
