#!/usr/bin/env bash
# The chart's static checks (CI: .github/workflows/helm.yml, on every pull
# request touching deploy/helm): helm lint, then each values file in
# test/values rendered and validated by kubeconform against the Kubernetes
# 1.30 and newest schemas (strict: unknown fields fail), the chart's
# validation refusing what it must, and the scripts it ships.
#
#   deploy/helm/test/lint.sh
#
# Needs helm (3.14 or later, or 4), kubeconform, and network for the
# schemas (KUBECONFORM_CACHE caches them). shellcheck when installed.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
chart=$(cd "$here/../romulus" && pwd)
root=$(cd "$here/../../.." && pwd)
versions=${KUBE_VERSIONS:-"1.30.0 1.37.0"}

step() { printf '\n== %s\n' "$*"; }
pass() { printf '   ok: %s\n' "$*"; }
fail() {
    printf 'lint: FAIL: %s\n' "$*" >&2
    exit 1
}

work=$(mktemp -d "${TMPDIR:-/tmp}/romulus-helm-lint.XXXXXX")
trap 'rm -rf "$work"' EXIT
cache=${KUBECONFORM_CACHE:-$work/schemas}
mkdir -p "$cache"

step "one version for romulus and its chart (RELEASING.md)"
chart_version=$(sed -n 's/^version: //p' "$chart/Chart.yaml")
app_version=$(sed -n 's/^appVersion: "\(.*\)"$/\1/p' "$chart/Chart.yaml")
[ "$chart_version" = "$app_version" ] ||
    fail "Chart.yaml: version $chart_version and appVersion $app_version differ: a release bumps both"
pass "$chart_version"

step "the chart ships deploy/self-host/backup.sh unchanged"
cmp -s "$root/deploy/self-host/backup.sh" "$chart/files/backup.sh" ||
    fail "deploy/helm/romulus/files/backup.sh differs from deploy/self-host/backup.sh: copy it over"
pass "files/backup.sh"

if command -v shellcheck >/dev/null 2>&1; then
    step "shellcheck"
    shellcheck "$chart/files/backup.sh" "$chart/files/restore.sh" "$here/lint.sh" "$here/e2e.sh"
    pass "the scripts"
fi

for values in "$here"/values/*.yaml; do
    name=$(basename "$values" .yaml)
    step "$name"
    helm lint --strict "$chart" -f "$values" >"$work/lint.log" 2>&1 || {
        cat "$work/lint.log" >&2
        fail "helm lint ($name)"
    }
    pass "helm lint --strict"
    helm template romulus "$chart" --namespace romulus -f "$values" >"$work/$name.yaml" ||
        fail "helm template ($name)"
    for version in $versions; do
        kubeconform -strict -summary -cache "$cache" -kubernetes-version "$version" "$work/$name.yaml" ||
            fail "kubeconform $version ($name)"
    done
    pass "kubeconform $versions"
    # Pod Security "restricted", the parts a manifest shows: no root, no
    # privilege escalation, every capability dropped, RuntimeDefault seccomp.
    if grep -nE 'privileged: true|allowPrivilegeEscalation: true|hostPath:|hostNetwork: true|hostPID: true|runAsUser: 0$' "$work/$name.yaml"; then
        fail "a pod asks for host access or privileges ($name)"
    fi
    contexts=$(grep -cE '^ +securityContext:$' "$work/$name.yaml" || true)
    images=$(grep -cE '^ +image: ' "$work/$name.yaml" || true)
    nonroot=$(grep -cE 'runAsNonRoot: true' "$work/$name.yaml" || true)
    seccomp=$(grep -cE 'type: RuntimeDefault' "$work/$name.yaml" || true)
    drops=$(grep -cE 'drop: \[ALL\]' "$work/$name.yaml" || true)
    readonly=$(grep -cE 'readOnlyRootFilesystem: true' "$work/$name.yaml" || true)
    if [ "$nonroot" != "$seccomp" ] || [ "$drops" != "$images" ] || [ "$readonly" != "$images" ] ||
        [ $((nonroot + images)) != "$contexts" ]; then
        fail "$name: every pod non-root with RuntimeDefault seccomp, every container read-only without capabilities ($contexts securityContexts, $nonroot pods, $images containers, $drops drop ALL, $readonly read-only)"
    fi
    pass "$nonroot pods, $images containers: non-root, read-only, no capabilities"
done

step "what the chart refuses"
refuses() {
    local why=$1 out
    shift
    if out=$(helm template romulus "$chart" "$@" 2>&1); then
        fail "rendered, but should refuse: $why ($*)"
    fi
    [[ $out == *"$why"* ]] || fail "refused, but not for \"$why\": $out"
    pass "$why"
}
refuses "set host" --set host=
refuses "the Ingress needs host" --set publicUrl=https://romulus.example.com
refuses "an origin without a path" --set host=romulus.example.com --set publicUrl=https://romulus.example.com/app
refuses "playgrounds are not supported on Kubernetes yet" --set host=romulus.example.com --set playgrounds.enabled=true
refuses "auth.methods must include password" --set host=romulus.example.com --set auth.methods=oidc
refuses "set auth.oidc.issuer" --set host=romulus.example.com --set 'auth.methods=password\,oidc'
refuses "open needs mail" --set host=romulus.example.com --set auth.signup=open
refuses "set smtp.from too" --set host=romulus.example.com --set smtp.host=smtp.example.com
refuses "development passwords" --set host=romulus.example.com --set postgresql.external.enabled=true \
    --set postgresql.external.host=db --set postgresql.external.password=romulus
refuses "set postgresql.external.host" --set host=romulus.example.com --set postgresql.external.enabled=true
refuses "value must be one of" --set host=romulus.example.com --set 'sandbox.extraMajors={14}'
refuses "additional properties" --set host=romulus.example.com --set hots=romulus.example.com
refuses "set admin.email" --set host=romulus.example.com --set admin.setupJob=true

step "what the chart renders"
renders() {
    local what=$1 pattern=$2 file=$3
    grep -qE -- "$pattern" "$work/$file.yaml" || fail "$file: $what (no line matching $pattern)"
    pass "$file: $what"
}
renders "the API's image is the chart's appVersion" "image: ghcr.io/lupidae/romulus-api:$(sed -n 's/^appVersion: "\(.*\)"$/\1/p' "$chart/Chart.yaml")$" default
renders "one sandbox per major" 'image: ghcr.io/lupidae/romulus-sandbox:0.1.0-pg18$' full
renders "18's data one level up" 'mountPath: /var/lib/postgresql$' full
renders "the generated Secret is kept on uninstall" 'helm.sh/resource-policy: keep' default
renders "the API is replaced, never doubled" 'type: Recreate' default
url=$(sed -n 's/^ *database-url: //p' "$work/external-db.yaml" | base64 --decode)
[ "$url" = 'postgres://romulus%40prod:p%40ss%20word%2Fwith%3Aodd%20chars%2B@db.internal.example.com:6432/romulus_prod?sslmode=verify-full' ] ||
    fail "external-db: the database URL is $url"
pass "external-db: the URL-encoded database URL"
renders "a database URL from a Secret's key" 'key: uri$' existing-secrets
renders "cert-manager's issuer" 'cert-manager.io/cluster-issuer: letsencrypt' full
grep -q 'kind: Job' "$work/default.yaml" && fail "default: a Job without setupJob or an upgrade"
pass "default: no Job on install (the pre-upgrade dump renders on upgrades only)"

printf '\nlint: all passed\n'
