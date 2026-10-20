#!/usr/bin/env bash
# Installs and operates romulus on this host (compose.yaml beside this file).
# docs/self-host/install.md is the guide. Re-run it any time: it converges.
#
#   ./install.sh [install] [OPTIONS]   check the host, ask (or read options), write .env once,
#                                      pull, start, create the admin and the organization,
#                                      print the admin's one-time sign-in link
#   ./install.sh upgrade [VERSION]     back up, then move to VERSION (default: this script's)
#   ./install.sh backup [DIR]          dump the app database and copy .env into DIR (./backups)
#   ./install.sh restore FILE [--version V]
#                                      the app database becomes FILE; the current one is dumped
#                                      first and kept as romulus_before_restore
#   ./install.sh status                containers and /api/health
#   ./install.sh logs [SERVICE…]       follow the logs (TAIL=n lines first, default 200)
#   ./install.sh admin-link EMAIL      a fresh one-time link to set EMAIL's password
#   ./install.sh uninstall [--delete-data [--confirm NAME]]
#                                      stop everything; volumes stay unless --delete-data
#
# Install options; each is also read from the environment variable in
# brackets, and from .env on a re-run (options win, then the environment):
#   --non-interactive        [ROMULUS_NONINTERACTIVE=1] never ask; fail on what is missing
#   --url URL                [ROMULUS_PUBLIC_URL] https://romulus.example.com, or http://localhost:8080
#   --admin-email EMAIL      [ROMULUS_ADMIN_EMAIL]
#   --admin-name NAME        [ROMULUS_ADMIN_NAME] default: the email's local part
#   --org NAME               [ROMULUS_ORG_NAME]
#   --org-slug SLUG          [ROMULUS_ORG_SLUG] default: derived from the name
#   --caddy | --no-caddy     [ROMULUS_CADDY=yes|no] bundled HTTPS (https URLs only)
#   --majors LIST            [ROMULUS_MAJORS] other Postgres majors to compile for: 15,16,18 or none
#   --playgrounds | --no-playgrounds   [ROMULUS_PLAYGROUNDS=yes|no] Linux, gVisor, an XFS volume
#   --play-dir DIR           [ROMULUS_PLAY_DIR] default /var/lib/romulus-play
#   --smtp-host HOST         [ROMULUS_SMTP_HOST] mail; --no-smtp turns it off
#   --smtp-port PORT         [ROMULUS_SMTP_PORT]
#   --smtp-tls MODE          [ROMULUS_SMTP_TLS] starttls (default) or implicit (sign-in
#                            links never travel in clear)
#   --smtp-username USER     [ROMULUS_SMTP_USERNAME] (the password: ROMULUS_SMTP_PASSWORD, or asked)
#   --mail-from ADDRESS      [ROMULUS_MAIL_FROM] e.g. "romulus <romulus@example.com>"
#   --mail-test              send a test email to the admin once running
#   --signup invite|open     [ROMULUS_SIGNUP] who may create an account (open needs SMTP)
#   --version VERSION        [ROMULUS_VERSION] default: this script's
#   --registry REGISTRY      [ROMULUS_REGISTRY] default ghcr.io/lupidae
#   --bind ADDRESS           [ROMULUS_BIND] where the API's port is published, default 127.0.0.1
#   --port PORT              [ROMULUS_PORT] default 8080
#   --project NAME           [COMPOSE_PROJECT_NAME] default romulus (volumes are named after it)
#   --no-pull                use images already on this machine
#
# It never runs sudo, apt or anything outside Docker: what the host lacks,
# it prints. Secrets are generated once into .env (mode 600) and never
# replaced.
set -euo pipefail

# The version `install` and `upgrade` use when none is given: `latest`, the
# newest release (each vX.Y.Z tag publishes X.Y.Z, X.Y and latest:
# RELEASING.md). --version X.Y.Z pins one.
ROMULUS_DEFAULT_VERSION=latest

DEFAULT_REGISTRY=ghcr.io/lupidae
DEFAULT_PLAY_DIR=/var/lib/romulus-play
DEFAULT_SUBNET=172.30.80.0/24
ALL_PROFILES="caddy pg15 pg16 pg18 playgrounds"
OTHER_MAJORS="15 16 18"

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ENV_FILE=$HERE/.env

say() { printf '%s\n' "$*"; }
warn() { printf 'install.sh: warning: %s\n' "$*" >&2; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

# --- .env: KEY='value' lines, literal for compose and for this script --------

env_get() {
    [ -f "$ENV_FILE" ] || return 0
    local line
    line=$(grep -E "^$1=" "$ENV_FILE" | tail -n 1) || return 0
    line=${line#*=}
    case $line in
    \'*\') line=${line#\'} && line=${line%\'} ;;
    \"*\") line=${line#\"} && line=${line%\"} ;;
    esac
    printf '%s' "$line"
}

# Replaces KEY's line in place (or appends it): comments, order and lines
# someone added by hand stay.
env_set() {
    local key=$1 value=$2 tmp
    case $value in
    *"'"* | *$'\n'*) die "$key: single quotes and newlines cannot go in .env" ;;
    esac
    tmp=$(umask 077 && mktemp "$ENV_FILE.XXXXXX")
    if [ -f "$ENV_FILE" ] && grep -qE "^$key=" "$ENV_FILE"; then
        KEY=$key LINE="$key='$value'" awk '
            index($0, ENVIRON["KEY"] "=") == 1 { if (!done) print ENVIRON["LINE"]; done = 1; next }
            { print }' "$ENV_FILE" >"$tmp"
    else
        { if [ -f "$ENV_FILE" ]; then cat "$ENV_FILE"; fi; printf "%s='%s'\n" "$key" "$value"; } >"$tmp"
    fi
    chmod 600 "$tmp"
    mv "$tmp" "$ENV_FILE"
}

# Every setting install.sh owns, documented, empty until it fills them.
create_env_file() {
    local tmp
    tmp=$(umask 077 && mktemp "$ENV_FILE.XXXXXX")
    cat >"$tmp" <<'EOF'
# romulus self-host settings (docs/self-host/configuration.md lists them all).
# install.sh wrote this file and changes only the lines its questions answer:
# it never replaces a secret and keeps lines you add. After an edit, run
# ./install.sh again (or `docker compose up -d` in this directory).
#
# Mode 600: this file holds every secret of this install. Keep a copy off
# this host with your backups: with it, a rebuilt host comes back as it was.

# --- Images
ROMULUS_VERSION=''
ROMULUS_REGISTRY=''

# --- Where romulus answers
ROMULUS_PUBLIC_URL=''
ROMULUS_DOMAIN=''
ROMULUS_BIND=''
ROMULUS_PORT=''
ROMULUS_SUBNET=''
# Derived from the above by install.sh: Caddy's fixed address, and whose
# X-Forwarded-For the API believes (ROMULUS_TRUSTED_PROXIES below overrides).
ROMULUS_CADDY_IP=''
ROMULUS_PROXIES=''

# --- What runs: caddy, pg15, pg16, pg18, playgrounds (compose profiles)
COMPOSE_PROFILES=''
ROMULUS_SANDBOX_PG15=''
ROMULUS_SANDBOX_PG16=''
ROMULUS_SANDBOX_PG18=''

# --- The first admin and the organization (`romulus-api setup`)
ROMULUS_ADMIN_EMAIL=''
ROMULUS_ADMIN_NAME=''
ROMULUS_ORG_NAME=''
ROMULUS_ORG_SLUG=''

# --- Accounts and mail (empty host: mail off)
ROMULUS_SIGNUP=''
ROMULUS_SMTP_HOST=''
ROMULUS_SMTP_PORT=''
ROMULUS_SMTP_TLS=''
ROMULUS_SMTP_USERNAME=''
ROMULUS_SMTP_PASSWORD=''
ROMULUS_MAIL_FROM=''

# --- Playgrounds (empty URL: off)
ROMULUS_PLAY_URL=''
ROMULUS_PLAY_DIR=''

# --- Backups (the `backup` service, `install.sh backup`)
ROMULUS_BACKUP_DIR=''
ROMULUS_UID=''
ROMULUS_GID=''

# --- Secrets, generated once
POSTGRES_PASSWORD=''
SANDBOX_PASSWORD=''
ROMULUS_PLAY_KEY=''
ROMULUS_PLAY_SECRET=''

# --- Optional: uncomment to change
# ROMULUS_API_MEMORY='1g'
# ROMULUS_POSTGRES_MEMORY='768m'
# ROMULUS_SANDBOX_MEMORY='768m'
# ROMULUS_PLAY_MAX_RUNNING='2'
# ROMULUS_TRUSTED_PROXIES=''        # default: ROMULUS_PROXIES (Caddy's address, or the gateway for a proxy on this host)
# BACKUP_AT='03:00'
# BACKUP_KEEP_DAYS='14'
# RUST_LOG='romulus_api=info,tower_http=info,sqlx=warn'
EOF
    mv "$tmp" "$ENV_FILE"
}

random_hex() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex "$1"
    else
        od -An -tx1 -N"$1" /dev/urandom | tr -d ' \n'
    fi
}

# --- docker compose, with .env as the only source of its variables ----------

# The caller's environment would win over .env in compose's interpolation:
# every variable compose.yaml reads, and every key .env sets, is unset
# first, so what runs is what .env says (a RUST_LOG left in a developer's
# shell included).
compose() {
    (
        for var in $(compgen -e); do
            case $var in
            ROMULUS_* | COMPOSE_* | BACKUP_* | POSTGRES_PASSWORD | SANDBOX_PASSWORD | RUST_LOG) unset "$var" ;;
            esac
        done
        if [ -f "$ENV_FILE" ]; then
            while IFS= read -r key; do unset "$key"; done < <(sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' "$ENV_FILE")
        fi
        exec docker compose --project-directory "$HERE" -f "$HERE/compose.yaml" --env-file "$ENV_FILE" "$@"
    )
}

project_name() {
    local name
    name=$(env_get COMPOSE_PROJECT_NAME)
    printf '%s' "${name:-romulus}"
}

require_env() {
    [ -f "$ENV_FILE" ] || die "no .env beside install.sh: run ./install.sh first"
}

# --- small helpers -------------------------------------------------------------

yes_no() {
    case $(printf '%s' "$1" | tr '[:upper:]' '[:lower:]') in
    y | yes | true | 1 | on) echo yes ;;
    n | no | false | 0 | off) echo no ;;
    *) echo "" ;;
    esac
}

in_list() { case " $2 " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# The first non-empty of: an option, the environment variable NAME, .env's NAME.
pick() {
    local option=$1 name=$2
    if [ -n "$option" ]; then
        printf '%s' "$option"
    elif [ -n "${!name:-}" ]; then
        printf '%s' "${!name}"
    else
        env_get "$name"
    fi
}

interactive() { [ -z "$NONINTERACTIVE" ]; }

# ask VAR QUESTION [DEFAULT] [secret]
ask() {
    local __ask_var=$1 __ask_q=$2 __ask_default=${3:-} __ask_answer=""
    [ -z "$__ask_default" ] || __ask_q="$__ask_q [$__ask_default]"
    if [ "${4:-}" = secret ]; then
        read -rs -p "$__ask_q: " __ask_answer || die "no answer (stdin closed): use --non-interactive and options"
        echo >&2
    else
        read -r -p "$__ask_q: " __ask_answer || die "no answer (stdin closed): use --non-interactive and options"
    fi
    printf -v "$__ask_var" '%s' "${__ask_answer:-$__ask_default}"
}

# ask_yn VAR QUESTION DEFAULT(yes|no)
ask_yn() {
    local __yn_var=$1 __yn_q=$2 __yn_default=$3 __yn_answer __yn_hint
    if [ "$__yn_default" = yes ]; then __yn_hint="Y/n"; else __yn_hint="y/N"; fi
    while :; do
        read -r -p "$__yn_q [$__yn_hint]: " __yn_answer || die "no answer (stdin closed): use --non-interactive and options"
        __yn_answer=$(yes_no "${__yn_answer:-$__yn_default}")
        [ -z "$__yn_answer" ] || break
        say "Please answer yes or no."
    done
    printf -v "$__yn_var" '%s' "$__yn_answer"
}

valid_url() {
    local re='^https?://(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?)(:[0-9]{1,5})?$'
    [[ $1 =~ $re ]]
}
url_scheme() { printf '%s' "${1%%://*}"; }
url_host() {
    local rest=${1#*://}
    case $rest in
    \[*) rest=${rest%%]*} && printf '%s' "${rest#\[}" ;;
    *) printf '%s' "${rest%%:*}" ;;
    esac
}
url_has_port() { local rest=${1#*://}; [[ ${rest##*]} == *:* ]]; }
# 127.0.0.1, ::1 or localhost: only this host reaches the API's port.
is_loopback_bind() {
    case $1 in
    "" | 127.* | ::1 | "[::1]" | localhost) return 0 ;;
    *) return 1 ;;
    esac
}
is_local_host() {
    case $1 in
    localhost | *.localhost | 127.* | ::1) return 0 ;;
    *) return 1 ;;
    esac
}
valid_email() { [[ $1 =~ ^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$ ]]; }
plain_text() { [ -n "$1" ] && [[ $1 != *"'"* ]] && [[ $1 != *$'\n'* ]]; }

# Something already answers on HOST:PORT.
port_in_use() { (exec 3<>"/dev/tcp/$1/$2") 2>/dev/null; }

running_services() {
    [ -f "$ENV_FILE" ] || return 0
    compose ps --status running --services 2>/dev/null || true
}

# --- the host -------------------------------------------------------------------

OS=""
check_host() {
    case $(uname -s) in
    Linux) OS=linux ;;
    Darwin)
        OS=macos
        warn "macOS: fine for a trial or development, not for production; playgrounds need Linux"
        ;;
    *) die "romulus installs on Linux (or macOS, for a trial)" ;;
    esac
    command -v docker >/dev/null 2>&1 || die "docker is not installed: https://docs.docker.com/engine/install/"
    local version major minor
    version=$(docker compose version --short 2>/dev/null) ||
        die "docker compose v2 is missing (the plugin, not docker-compose v1): https://docs.docker.com/compose/install/linux/"
    version=${version#v}
    major=${version%%.*}
    minor=${version#*.}
    minor=${minor%%.*}
    if ! [[ $major =~ ^[0-9]+$ && $minor =~ ^[0-9]+$ ]] || ((major < 2 || (major == 2 && minor < 20))); then
        die "docker compose $version is too old: 2.20 or later"
    fi
    docker info >/dev/null 2>&1 ||
        die "cannot reach the Docker daemon: is it running, and may this user use it (the docker group, or sudo)?"
    local registry
    registry=$(pick "$OPT_REGISTRY" ROMULUS_REGISTRY)
    case $(uname -m) in
    arm64 | aarch64)
        if [ -z "$registry" ] || [ "$registry" = "$DEFAULT_REGISTRY" ]; then
            warn "the published images are for amd64: on this arm64 host they run emulated, slowly, if at all"
        fi
        ;;
    esac
}

# Memory and disk: warnings, never a refusal (but for a nearly full disk).
check_resources() {
    local majors=$1 playgrounds=$2 caddy=$3 mem mem_mb need avail_kb root
    need=2800
    [ "$caddy" = no ] || need=$((need + 128))
    for _ in $majors; do need=$((need + 768)); done
    [ "$playgrounds" = no ] || need=$((need + 1700))
    mem=$(docker info --format '{{.MemTotal}}' 2>/dev/null || echo 0)
    [[ $mem =~ ^[0-9]+$ ]] || mem=0
    mem_mb=$((mem / 1048576))
    if ((mem > 0 && mem < 3700000000)); then
        warn "Docker has ${mem_mb} MB of memory; romulus wants a 4 GB host at least (docs/self-host/install.md, \"Sizing\")"
    elif ((mem > 0 && mem_mb < need + 512)); then
        warn "the memory limits of what you chose add up to about ${need} MB and Docker has ${mem_mb} MB: fine while idle, tight under load"
    fi
    root=$(docker info --format '{{.DockerRootDir}}' 2>/dev/null || true)
    [ -n "$root" ] && [ -d "$root" ] || root=$HERE
    avail_kb=$(df -Pk "$root" 2>/dev/null | awk 'NR == 2 { print $4 }')
    [[ $avail_kb =~ ^[0-9]+$ ]] || return 0
    if ((avail_kb < 3 * 1024 * 1024)); then
        die "only $((avail_kb / 1024)) MB free for Docker ($root): the images alone need about 3 GB"
    elif ((avail_kb < 10 * 1024 * 1024)); then
        warn "$((avail_kb / 1024)) MB free for Docker ($root): 10 GB or more is advised (images, the database, backups)"
    fi
}

# The API's port is taken by something else than this install's API.
api_port_taken() {
    local host=$1
    case $host in 0.0.0.0 | "" | "::") host=127.0.0.1 ;; esac
    in_list api "$(running_services | tr '\n' ' ')" && return 1
    port_in_use "$host" "$2"
}

check_ports() {
    local caddy=$1 running
    running=$(running_services)
    if [ "$caddy" = yes ] && ! in_list caddy "$(echo "$running" | tr '\n' ' ')"; then
        local p
        for p in 80 443; do
            port_in_use 127.0.0.1 "$p" && die "port $p is taken: the bundled Caddy needs 80 and 443 (or use your own proxy: --no-caddy)"
        done
    fi
    return 0
}

# Prints what the playground host lacks, with exactly what to run; returns 1
# when something is missing.
check_playground_host() {
    local dir=$1 runtimes fstype options missing_runsc="" missing_runtimes="" missing_xfs=""
    runtimes=$(docker info --format '{{json .Runtimes}}' 2>/dev/null || true)
    if [[ $runtimes != *'"runsc"'* || $runtimes != *'"runsc-uds"'* || $runtimes != *'--host-uds=create'* ]]; then
        missing_runtimes=yes
        command -v runsc >/dev/null 2>&1 || [ -x /usr/bin/runsc ] || missing_runsc=yes
    fi
    if [ ! -d "$dir" ]; then
        missing_xfs="$dir does not exist"
    elif ! command -v findmnt >/dev/null 2>&1; then
        missing_xfs="findmnt is missing (package util-linux): cannot check $dir"
    else
        # One column at a time: findmnt pads columns.
        fstype=$(findmnt -no FSTYPE --target "$dir" 2>/dev/null | tr -d ' ' || true)
        options=$(findmnt -no OPTIONS --target "$dir" 2>/dev/null | tr -d ' ' || true)
        if [ "$fstype" != xfs ]; then
            missing_xfs="$dir is not on XFS (${fstype:-unknown})"
        else
            case ",$options," in
            *,prjquota,* | *,pquota,*) ;;
            *) missing_xfs="$dir is on XFS without project quotas (mount option prjquota)" ;;
            esac
        fi
    fi
    [ -n "$missing_runsc$missing_runtimes$missing_xfs" ] || return 0

    say ""
    say "Playgrounds need gVisor and an XFS volume with project quotas; this host lacks:"
    [ -z "$missing_runsc" ] || say "  - gVisor (runsc)"
    [ -z "$missing_runtimes" ] || say "  - the Docker runtimes runsc and runsc-uds"
    [ -z "$missing_xfs" ] || say "  - $missing_xfs"
    say ""
    say "Run these yourself (Debian or Ubuntu; elsewhere see https://gvisor.dev/docs/user_guide/install/),"
    say "then run ./install.sh again. docs/self-host/playgrounds.md explains each step."
    if [ -n "$missing_runsc" ]; then
        cat <<'EOF'

# 1. gVisor, from its apt repository
sudo apt-get update && sudo apt-get install -y ca-certificates curl gnupg
curl -fsSL https://gvisor.dev/archive.key | sudo gpg --dearmor -o /usr/share/keyrings/gvisor-archive-keyring.gpg
echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/gvisor-archive-keyring.gpg] https://storage.googleapis.com/gvisor/releases release main" \
  | sudo tee /etc/apt/sources.list.d/gvisor.list >/dev/null
sudo apt-get update && sudo apt-get install -y runsc
EOF
    fi
    if [ -n "$missing_runtimes" ]; then
        cat <<'EOF'

# 2. Two Docker runtimes: put this in /etc/docker/daemon.json (merge it with
#    what the file already holds), then reload Docker (running containers are
#    untouched by a reload).
{
  "runtimes": {
    "runsc": { "path": "/usr/bin/runsc" },
    "runsc-uds": { "path": "/usr/bin/runsc", "runtimeArgs": ["--host-uds=create"] }
  }
}
sudo systemctl reload docker
EOF
    fi
    if [ -n "$missing_xfs" ]; then
        sed "s#@DIR@#$dir#g" <<'EOF'

# 3. An XFS volume with project quotas at @DIR@: a 12 GB sparse file
#    (disk is used as data grows), mounted at every boot.
sudo apt-get install -y xfsprogs
sudo truncate -s 12G @DIR@.img
sudo mkfs.xfs -m reflink=1 @DIR@.img
sudo mkdir -p @DIR@
echo '@DIR@.img @DIR@ xfs loop,prjquota,noatime 0 0' | sudo tee -a /etc/fstab
sudo mount @DIR@
EOF
    fi
    cat <<EOF

# Check
docker info --format '{{json .Runtimes}}'      # lists runsc and runsc-uds
findmnt -no FSTYPE,OPTIONS --target $dir       # xfs … prjquota
EOF
    say ""
    return 1
}

# --- health ---------------------------------------------------------------------

health_url() {
    local host port
    host=$(env_get ROMULUS_BIND)
    port=$(env_get ROMULUS_PORT)
    case $host in
    0.0.0.0 | "") host=127.0.0.1 ;;
    "::") host="[::1]" ;;
    *:*) host="[$host]" ;;
    esac
    printf 'http://%s:%s/api/health' "$host" "${port:-8080}"
}

health_body() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsS --max-time 5 "$(health_url)" 2>/dev/null
    else
        compose exec -T api curl -fsS --max-time 5 http://127.0.0.1:8080/api/health 2>/dev/null
    fi
}

HEALTH=""
# wait_for SECONDS PATTERN: until /api/health's body contains PATTERN.
wait_for() {
    local deadline=$((SECONDS + $1))
    while :; do
        HEALTH=$(health_body || true)
        [[ $HEALTH == *"$2"* ]] && return 0
        ((SECONDS < deadline)) || return 1
        sleep 2
    done
}

# Starts what .env says, stops what it no longer runs, waits for the API.
start_stack() {
    if ! compose up -d --wait --wait-timeout 600 --remove-orphans; then
        compose ps --all >&2 || true
        compose logs --tail 40 api >&2 || true
        return 1
    fi
    stop_disabled
    if ! wait_for 120 '"status":"ok"'; then
        warn "/api/health does not answer on $(health_url)"
        return 1
    fi
    [[ $HEALTH == *'"database":"ok"'* ]] || { warn "the API cannot use its database: $HEALTH"; return 1; }
    wait_for 120 '"sandbox":"ok"' ||
        warn "the SQL sandbox (Postgres 17) is not ready yet: uploads and drafts answer 503 until it is (docker compose logs sandbox)"
    if [ -n "$(env_get ROMULUS_PLAY_URL)" ]; then
        wait_for 60 '"playgrounds":"ok"' ||
            warn "the playground host is not ready: docker compose logs play-runner playground (docs/self-host/playgrounds.md)"
    fi
    return 0
}

stop_disabled() {
    local profiles profile services
    profiles=" $(env_get COMPOSE_PROFILES | tr ',' ' ') "
    for profile in $ALL_PROFILES; do
        case $profiles in *" $profile "*) continue ;; esac
        case $profile in
        caddy) services=caddy ;;
        pg15 | pg16 | pg18) services=sandbox-${profile#pg} ;;
        playgrounds) services="play-runner playground" ;;
        esac
        # shellcheck disable=SC2086 # several services
        compose --profile "$profile" rm --stop --force $services >/dev/null 2>&1 || true
    done
    if [[ $profiles != *" playgrounds "* ]]; then
        local scope ids
        scope=$(env_get ROMULUS_PLAY_SCOPE)
        ids=$(docker ps -q --filter "label=romulus.play.scope=${scope:-self-host}" 2>/dev/null || true)
        # shellcheck disable=SC2086 # one id per word
        [ -z "$ids" ] || docker stop $ids >/dev/null || true
    fi
}

pull_images() {
    [ -z "$NO_PULL" ] || return 0
    compose pull || return 1
    if [ -n "$(env_get ROMULUS_PLAY_URL)" ]; then
        # The runner starts playgrounds from it; it is no compose service.
        docker pull "$(env_get ROMULUS_REGISTRY)/romulus-playground:$(env_get ROMULUS_VERSION)" || return 1
    fi
}

# --- backups --------------------------------------------------------------------

backup_dir() {
    local dir
    dir=$(env_get ROMULUS_BACKUP_DIR)
    dir=${dir:-./backups}
    case $dir in /*) ;; *) dir=$HERE/${dir#./} ;; esac
    printf '%s' "$dir"
}

ensure_postgres() {
    compose up -d --wait postgres >/dev/null 2>&1 || die "the app database does not start: docker compose logs postgres"
}

# backup_to DIR LABEL → the dump's path on stdout. Written beside, checked to
# read back, then renamed: a failed dump never becomes a *.dump. .env is
# copied next to it (mode 600).
backup_to() {
    local dir=$1 label stamp base
    label=$(printf '%s' "$2" | tr -c 'A-Za-z0-9._-' '-')
    (umask 077 && mkdir -p "$dir") || return 1
    stamp=$(date -u +%Y%m%dT%H%M%SZ)
    base=$dir/romulus-$stamp-$label
    if ! (umask 077 && compose exec -T postgres pg_dump -U romulus -d romulus --format=custom --no-owner >"$base.dump.partial"); then
        rm -f "$base.dump.partial"
        warn "pg_dump failed; nothing written"
        return 1
    fi
    if ! compose exec -T postgres pg_restore --list <"$base.dump.partial" >/dev/null; then
        rm -f "$base.dump.partial"
        warn "the dump does not read back; nothing written"
        return 1
    fi
    chmod 600 "$base.dump.partial"
    mv "$base.dump.partial" "$base.dump"
    (umask 077 && cp "$ENV_FILE" "$base.env")
    printf '%s\n' "$base.dump"
}

# The app database becomes the dump on stdin: restored beside, then swapped
# in by renaming, so a failure before the swap changes nothing. The replaced
# database stays as romulus_before_restore (the previous one of that name is
# dropped).
restore_db() {
    compose exec -T postgres sh -c 'umask 077 && cat > /tmp/romulus-restore.dump' || return 1
    compose exec -T postgres bash -s <<'SH'
set -uo pipefail
dump=/tmp/romulus-restore.dump
trap 'rm -f "$dump"' EXIT
q() { PGOPTIONS='-c client_min_messages=warning' psql -X -q -v ON_ERROR_STOP=1 -U romulus -d postgres "$@"; }
log() { echo "restore: $*" >&2; }
q -c 'DROP DATABASE IF EXISTS romulus_restoring WITH (FORCE)' -c 'CREATE DATABASE romulus_restoring' || exit 1
if ! pg_restore -U romulus --no-owner --exit-on-error --single-transaction -d romulus_restoring "$dump"; then
    q -c 'DROP DATABASE IF EXISTS romulus_restoring WITH (FORCE)'
    log "pg_restore failed; the database is unchanged"
    exit 1
fi
q -c 'DROP DATABASE IF EXISTS romulus_before_restore WITH (FORCE)' || exit 1
# Nobody connects while the names move (the API is stopped; the backup
# service might not be).
q -c 'ALTER DATABASE romulus ALLOW_CONNECTIONS false' || exit 1
renamed=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
    q -tA -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = 'romulus'" >/dev/null
    if q -c 'ALTER DATABASE romulus RENAME TO romulus_before_restore' 2>/dev/null; then renamed=yes; break; fi
    sleep 1
done
if [ -z "$renamed" ]; then
    q -c 'ALTER DATABASE romulus ALLOW_CONNECTIONS true'
    q -c 'DROP DATABASE IF EXISTS romulus_restoring WITH (FORCE)'
    log "the database stayed in use; it is unchanged"
    exit 1
fi
q -c 'ALTER DATABASE romulus_before_restore ALLOW_CONNECTIONS true'
if ! q -c 'ALTER DATABASE romulus_restoring RENAME TO romulus'; then
    q -c 'ALTER DATABASE romulus_before_restore RENAME TO romulus'
    log "could not swap the restored database in; the database is unchanged"
    exit 1
fi
log "done; the replaced database is romulus_before_restore"
SH
}

# --- install --------------------------------------------------------------------

NONINTERACTIVE=${ROMULUS_NONINTERACTIVE:-}
case $NONINTERACTIVE in 0 | false | no) NONINTERACTIVE="" ;; esac
NO_PULL=""
OPT_URL="" OPT_EMAIL="" OPT_NAME="" OPT_ORG="" OPT_SLUG="" OPT_CADDY="" OPT_MAJORS=""
OPT_PLAYGROUNDS="" OPT_PLAY_DIR="" OPT_SMTP_HOST="" OPT_SMTP_PORT="" OPT_SMTP_TLS=""
OPT_SMTP_USERNAME="" OPT_MAIL_FROM="" OPT_NO_SMTP="" OPT_MAIL_TEST="" OPT_SIGNUP=""
OPT_VERSION="" OPT_REGISTRY="" OPT_BIND="" OPT_PORT="" OPT_PROJECT=""

parse_install_options() {
    while [ $# -gt 0 ]; do
        local option=$1 value=${2:-}
        case $option in
        --non-interactive) NONINTERACTIVE=1 ;;
        --no-pull) NO_PULL=1 ;;
        --caddy) OPT_CADDY=yes ;;
        --no-caddy) OPT_CADDY=no ;;
        --playgrounds) OPT_PLAYGROUNDS=yes ;;
        --no-playgrounds) OPT_PLAYGROUNDS=no ;;
        --no-smtp) OPT_NO_SMTP=1 ;;
        --mail-test) OPT_MAIL_TEST=1 ;;
        -h | --help) usage; exit 0 ;;
        --url | --admin-email | --admin-name | --org | --org-slug | --majors | --play-dir | --smtp-host | \
            --smtp-port | --smtp-tls | --smtp-username | --mail-from | --signup | --version | --registry | \
            --bind | --port | --project)
            [ $# -ge 2 ] || die "$option needs a value"
            case $option in
            --url) OPT_URL=$value ;;
            --admin-email) OPT_EMAIL=$value ;;
            --admin-name) OPT_NAME=$value ;;
            --org) OPT_ORG=$value ;;
            --org-slug) OPT_SLUG=$value ;;
            --majors) OPT_MAJORS=$value ;;
            --play-dir) OPT_PLAY_DIR=$value ;;
            --smtp-host) OPT_SMTP_HOST=$value ;;
            --smtp-port) OPT_SMTP_PORT=$value ;;
            --smtp-tls) OPT_SMTP_TLS=$value ;;
            --smtp-username) OPT_SMTP_USERNAME=$value ;;
            --mail-from) OPT_MAIL_FROM=$value ;;
            --signup) OPT_SIGNUP=$value ;;
            --version) OPT_VERSION=$value ;;
            --registry) OPT_REGISTRY=$value ;;
            --bind) OPT_BIND=$value ;;
            --port) OPT_PORT=$value ;;
            --project) OPT_PROJECT=$value ;;
            esac
            shift
            ;;
        *) die "unknown option $option (./install.sh --help)" ;;
        esac
        shift
    done
}

# Each setting: the options, the environment, .env, then a question (or a
# default when not interactive). Sets the S_* variables.
S_VERSION="" S_REGISTRY="" S_BIND="" S_PORT="" S_PROJECT="" S_URL="" S_EMAIL="" S_NAME=""
S_ORG="" S_SLUG="" S_CADDY="" S_MAJORS="" S_SIGNUP="" S_SMTP_HOST="" S_SMTP_PORT="" S_SMTP_TLS=""
S_SMTP_USERNAME="" S_SMTP_PASSWORD="" S_MAIL_FROM="" S_PLAYGROUNDS="" S_PLAY_DIR="" S_SUBNET=""

resolve_settings() {
    S_VERSION=$(pick "$OPT_VERSION" ROMULUS_VERSION)
    S_VERSION=${S_VERSION:-$ROMULUS_DEFAULT_VERSION}
    S_REGISTRY=$(pick "$OPT_REGISTRY" ROMULUS_REGISTRY)
    S_REGISTRY=${S_REGISTRY:-$DEFAULT_REGISTRY}
    S_BIND=$(pick "$OPT_BIND" ROMULUS_BIND)
    S_BIND=${S_BIND:-127.0.0.1}
    S_PORT=$(pick "$OPT_PORT" ROMULUS_PORT)
    S_PORT=${S_PORT:-8080}
    if ! [[ $S_PORT =~ ^[0-9]+$ ]] || ((S_PORT < 1 || S_PORT > 65535)); then
        die "--port: a port number, not $S_PORT"
    fi
    if api_port_taken "$S_BIND" "$S_PORT"; then
        interactive || die "port $S_PORT is taken on $S_BIND: free it, or choose another with --port"
        say "Port $S_PORT is taken on this host; romulus's API needs one of its own."
        while :; do
            ask S_PORT "Port for romulus (on $S_BIND)" "$((S_PORT + 1))"
            [[ $S_PORT =~ ^[0-9]+$ ]] && ((S_PORT > 0 && S_PORT < 65536)) && ! api_port_taken "$S_BIND" "$S_PORT" && break
            say "That one is taken (or not a port)."
        done
    fi
    S_PROJECT=$(pick "$OPT_PROJECT" COMPOSE_PROJECT_NAME)
    [ -z "$S_PROJECT" ] || [[ $S_PROJECT =~ ^[a-z0-9][a-z0-9_-]*$ ]] ||
        die "--project: lowercase letters, digits, - and _ only"
    S_SUBNET=$(pick "" ROMULUS_SUBNET)
    S_SUBNET=${S_SUBNET:-$DEFAULT_SUBNET}

    # The public URL.
    S_URL=$(pick "$OPT_URL" ROMULUS_PUBLIC_URL)
    S_URL=${S_URL%/}
    if interactive; then
        say ""
        say "The public URL is where people open romulus: https://romulus.example.com behind"
        say "TLS, or http://localhost:$S_PORT for a trial on this machine."
        while :; do
            ask S_URL "Public URL" "${S_URL:-http://localhost:$S_PORT}"
            S_URL=${S_URL%/}
            valid_url "$S_URL" && break
            say "A URL like https://romulus.example.com (no path)."
        done
    else
        [ -n "$S_URL" ] || die "the public URL is missing: --url (or ROMULUS_PUBLIC_URL)"
        valid_url "$S_URL" || die "--url: a URL like https://romulus.example.com (no path), not $S_URL"
    fi
    local scheme host
    scheme=$(url_scheme "$S_URL")
    host=$(url_host "$S_URL")

    # The admin and the organization.
    S_EMAIL=$(pick "$OPT_EMAIL" ROMULUS_ADMIN_EMAIL)
    S_NAME=$(pick "$OPT_NAME" ROMULUS_ADMIN_NAME)
    S_ORG=$(pick "$OPT_ORG" ROMULUS_ORG_NAME)
    S_SLUG=$(pick "$OPT_SLUG" ROMULUS_ORG_SLUG)
    if interactive; then
        say ""
        say "The first admin (you, probably) and the organization everyone joins."
        while :; do
            ask S_EMAIL "Admin's email" "$S_EMAIL"
            valid_email "$S_EMAIL" && plain_text "$S_EMAIL" && break
            say "An email address, please."
        done
        while :; do
            ask S_NAME "Admin's name" "${S_NAME:-${S_EMAIL%%@*}}"
            plain_text "$S_NAME" && break
            say "A name, without single quotes."
        done
        while :; do
            ask S_ORG "Organization's name" "$S_ORG"
            plain_text "$S_ORG" && break
            say "A name, without single quotes."
        done
    else
        valid_email "$S_EMAIL" || die "the admin's email is missing or invalid: --admin-email (or ROMULUS_ADMIN_EMAIL)"
        S_NAME=${S_NAME:-${S_EMAIL%%@*}}
        plain_text "$S_NAME" || die "--admin-name: no single quotes"
        plain_text "$S_ORG" || die "the organization's name is missing: --org (or ROMULUS_ORG_NAME)"
    fi
    [ -z "$S_SLUG" ] || [[ $S_SLUG =~ ^[a-z0-9][a-z0-9-]*$ ]] || die "--org-slug: lowercase letters, digits and -"

    # Caddy: only for https on a real domain, on the default ports.
    local previous_caddy=""
    in_list caddy "$(env_get COMPOSE_PROFILES | tr ',' ' ')" && previous_caddy=yes
    S_CADDY=$(yes_no "$(pick "$OPT_CADDY" ROMULUS_CADDY)")
    [ -n "$S_CADDY" ] || S_CADDY=$previous_caddy
    if [ "$scheme" = https ] && ! is_local_host "$host" && ! url_has_port "$S_URL"; then
        if interactive; then
            say ""
            say "romulus needs TLS in front of it. The bundled Caddy gets a certificate for $host"
            say "from Let's Encrypt (DNS must point here, ports 80 and 443 open); or run your own"
            say "proxy (nginx, Caddy, a load balancer) to 127.0.0.1:$S_PORT."
            ask_yn S_CADDY "Run the bundled Caddy for HTTPS?" "${S_CADDY:-yes}"
        fi
        S_CADDY=${S_CADDY:-no}
    else
        if [ "$S_CADDY" = yes ] && [ -n "$OPT_CADDY${ROMULUS_CADDY:-}" ]; then
            die "--caddy needs an https URL on a domain, without a port (not $S_URL)"
        fi
        S_CADDY=no
    fi

    # The other Postgres majors.
    local previous_majors="" major
    for major in $OTHER_MAJORS; do
        [ -z "$(env_get "ROMULUS_SANDBOX_PG$major")" ] || previous_majors="$previous_majors $major"
    done
    S_MAJORS=$(pick "$OPT_MAJORS" ROMULUS_MAJORS)
    [ -n "$S_MAJORS" ] || S_MAJORS=${previous_majors# }
    if interactive; then
        say ""
        say "Databases target Postgres 17 by default. A database on 15, 16 or 18 compiles on"
        say "that major's own Postgres, about 768 MB of memory each."
        while :; do
            ask S_MAJORS "Other Postgres majors to run (e.g. 15,18, or none)" "${S_MAJORS:-none}"
            normalize_majors && break
            say "A list of 15, 16 and 18, or none."
        done
    else
        normalize_majors || die "--majors: a list of 15, 16 and 18, or none (not $S_MAJORS)"
    fi

    resolve_smtp
    S_SIGNUP=$(pick "$OPT_SIGNUP" ROMULUS_SIGNUP)
    S_SIGNUP=${S_SIGNUP:-invite}
    case $S_SIGNUP in
    invite) ;;
    open) [ -n "$S_SMTP_HOST" ] || die "--signup open needs SMTP: an account proves its address by email" ;;
    *) die "--signup: invite or open, not $S_SIGNUP" ;;
    esac

    resolve_playgrounds
}

normalize_majors() {
    local list out="" major
    list=$(printf '%s' "$S_MAJORS" | tr ',' ' ')
    case $list in none | "" | " ") S_MAJORS="" && return 0 ;; esac
    for major in $list; do
        in_list "$major" "$OTHER_MAJORS" || return 1
        in_list "$major" "$out" || out="$out $major"
    done
    S_MAJORS=${out# }
}

resolve_smtp() {
    S_SMTP_HOST=$(pick "$OPT_SMTP_HOST" ROMULUS_SMTP_HOST)
    S_SMTP_PORT=$(pick "$OPT_SMTP_PORT" ROMULUS_SMTP_PORT)
    S_SMTP_TLS=$(pick "$OPT_SMTP_TLS" ROMULUS_SMTP_TLS)
    S_SMTP_USERNAME=$(pick "$OPT_SMTP_USERNAME" ROMULUS_SMTP_USERNAME)
    S_SMTP_PASSWORD=$(pick "" ROMULUS_SMTP_PASSWORD)
    S_MAIL_FROM=$(pick "$OPT_MAIL_FROM" ROMULUS_MAIL_FROM)
    if [ -n "$OPT_NO_SMTP" ]; then
        S_SMTP_HOST="" S_SMTP_PORT="" S_SMTP_TLS="" S_SMTP_USERNAME="" S_SMTP_PASSWORD="" S_MAIL_FROM=""
        return 0
    fi
    if interactive; then
        local want
        say ""
        say "Mail (SMTP) is strongly recommended: it sends invitations, sign-up links and"
        say "password resets, and a reset link is how someone locked out after wrong passwords"
        say "gets back in on their own. Without it, invitation links are copied by hand and an"
        say "admin makes password links with ./install.sh admin-link EMAIL."
        say "Any provider works (Resend: smtp.resend.com, user resend, password = an API key)."
        if [ -n "$S_SMTP_HOST" ]; then
            ask_yn want "Keep mail through $S_SMTP_HOST?" yes
            if [ "$want" = yes ]; then return 0; fi
            ask_yn want "Set up another SMTP server (no: mail off)?" yes
        else
            ask_yn want "Set up SMTP now (recommended)?" yes
        fi
        if [ "$want" = no ]; then
            S_SMTP_HOST="" S_SMTP_PORT="" S_SMTP_TLS="" S_SMTP_USERNAME="" S_SMTP_PASSWORD="" S_MAIL_FROM=""
            return 0
        fi
        while :; do ask S_SMTP_HOST "SMTP host" "$S_SMTP_HOST"; plain_text "$S_SMTP_HOST" && break; done
        while :; do
            ask S_SMTP_TLS "TLS: starttls (port 587) or implicit (port 465)" "${S_SMTP_TLS:-starttls}"
            case $S_SMTP_TLS in starttls | implicit) break ;; esac
        done
        local default_port=587
        [ "$S_SMTP_TLS" != implicit ] || default_port=465
        while :; do ask S_SMTP_PORT "Port" "${S_SMTP_PORT:-$default_port}"; [[ $S_SMTP_PORT =~ ^[0-9]+$ ]] && break; done
        ask S_SMTP_USERNAME "Username (empty: none)" "$S_SMTP_USERNAME"
        if [ -n "$S_SMTP_USERNAME" ]; then
            local password
            while :; do
                if [ -n "$S_SMTP_PASSWORD" ]; then
                    ask password "Password (empty: keep the current one)" "" secret
                    password=${password:-$S_SMTP_PASSWORD}
                else
                    ask password "Password" "" secret
                fi
                [ -n "$password" ] && plain_text "$password" && break
                say "A password, without single quotes."
            done
            S_SMTP_PASSWORD=$password
        else
            S_SMTP_PASSWORD=""
        fi
        while :; do
            ask S_MAIL_FROM "From address (e.g. romulus <romulus@example.com>)" "$S_MAIL_FROM"
            [[ $S_MAIL_FROM == *@* ]] && plain_text "$S_MAIL_FROM" && break
        done
    fi
    [ -n "$S_SMTP_HOST" ] || { S_SMTP_PORT="" S_SMTP_TLS="" S_SMTP_USERNAME="" S_SMTP_PASSWORD="" S_MAIL_FROM=""; return 0; }
    # The API refuses mail in clear but to its own loopback, which, in its
    # container, has no relay: TLS it is.
    case ${S_SMTP_TLS:-starttls} in starttls | implicit) ;; *) die "--smtp-tls: starttls or implicit (sign-in links never travel in clear)" ;; esac
    [ -z "$S_SMTP_PORT" ] || [[ $S_SMTP_PORT =~ ^[0-9]+$ ]] || die "--smtp-port: a port number"
    [[ $S_MAIL_FROM == *@* ]] || die "SMTP needs a from address: --mail-from (or ROMULUS_MAIL_FROM)"
    if [ -n "$S_SMTP_USERNAME" ] && [ -z "$S_SMTP_PASSWORD" ]; then
        die "an SMTP username needs its password: ROMULUS_SMTP_PASSWORD in the environment"
    fi
    if [ -z "$S_SMTP_USERNAME" ] && [ -n "$S_SMTP_PASSWORD" ]; then
        die "an SMTP password needs its username: --smtp-username"
    fi
    local value
    for value in "$S_SMTP_HOST" "$S_SMTP_USERNAME" "$S_SMTP_PASSWORD" "$S_MAIL_FROM"; do
        [ -z "$value" ] || plain_text "$value" || die "SMTP settings cannot contain single quotes"
    done
}

resolve_playgrounds() {
    local previous=""
    [ -z "$(env_get ROMULUS_PLAY_URL)" ] || previous=yes
    S_PLAYGROUNDS=$(yes_no "$(pick "$OPT_PLAYGROUNDS" ROMULUS_PLAYGROUNDS)")
    [ -n "$S_PLAYGROUNDS" ] || S_PLAYGROUNDS=$previous
    S_PLAY_DIR=$(pick "$OPT_PLAY_DIR" ROMULUS_PLAY_DIR)
    S_PLAY_DIR=${S_PLAY_DIR:-$DEFAULT_PLAY_DIR}
    S_PLAY_DIR=${S_PLAY_DIR%/}
    [[ $S_PLAY_DIR =~ ^/[A-Za-z0-9._/-]+$ ]] || die "--play-dir: an absolute path (letters, digits, . _ - /)"
    if [ "$OS" != linux ]; then
        [ "$S_PLAYGROUNDS" != yes ] || [ -z "$OPT_PLAYGROUNDS${ROMULUS_PLAYGROUNDS:-}" ] ||
            die "playgrounds need Linux (gVisor): not on $(uname -s)"
        S_PLAYGROUNDS=no
        return 0
    fi
    if interactive; then
        say ""
        say "Playgrounds are real, throwaway Postgres databases filled with generated rows, to"
        say "query and to try a migration on. Each runs isolated in gVisor (about 512 MB each)."
        ask_yn S_PLAYGROUNDS "Enable playgrounds? (needs Linux + gVisor + an XFS volume)" "${S_PLAYGROUNDS:-no}"
        if [ "$S_PLAYGROUNDS" = yes ]; then
            ask S_PLAY_DIR "Playground directory (on XFS with prjquota)" "$S_PLAY_DIR"
            S_PLAY_DIR=${S_PLAY_DIR%/}
            [[ $S_PLAY_DIR =~ ^/[A-Za-z0-9._/-]+$ ]] || die "the playground directory: an absolute path"
        fi
    fi
    S_PLAYGROUNDS=${S_PLAYGROUNDS:-no}
    [ "$S_PLAYGROUNDS" = yes ] || return 0
    check_playground_host "$S_PLAY_DIR" && return 0
    if ! interactive; then
        die "playgrounds were asked for but this host is not ready (above); or install with --no-playgrounds"
    fi
    local go_on
    ask_yn go_on "Continue now with playgrounds off (re-run ./install.sh to turn them on)? No stops here" no
    [ "$go_on" = yes ] || { say "Stopped: nothing changed. Set up the host as above, then run ./install.sh again."; exit 1; }
    S_PLAYGROUNDS=no
}

# Writes the settings; generates each secret only when .env has none.
write_env() {
    [ -f "$ENV_FILE" ] || create_env_file
    local profiles="" major key
    [ "$S_CADDY" != yes ] || profiles="caddy"
    for major in $OTHER_MAJORS; do
        if in_list "$major" "$S_MAJORS"; then
            profiles="$profiles,pg$major"
            env_set "ROMULUS_SANDBOX_PG$major" on
        else
            env_set "ROMULUS_SANDBOX_PG$major" ""
        fi
    done
    [ "$S_PLAYGROUNDS" != yes ] || profiles="$profiles,playgrounds"
    profiles=${profiles#,}

    env_set ROMULUS_VERSION "$S_VERSION"
    env_set ROMULUS_REGISTRY "$S_REGISTRY"
    env_set ROMULUS_PUBLIC_URL "$S_URL"
    env_set ROMULUS_DOMAIN "$(url_host "$S_URL")"
    env_set ROMULUS_BIND "$S_BIND"
    env_set ROMULUS_PORT "$S_PORT"
    env_set ROMULUS_SUBNET "$S_SUBNET"
    # Who may say who the client is (X-Forwarded-For): the bundled Caddy at
    # its fixed address; else a proxy on this host, which reaches the
    # loopback-published port through the network's gateway (Docker's
    # userland proxy); nobody when the port is published beyond loopback.
    # ROMULUS_TRUSTED_PROXIES, set by hand, wins over this.
    local net=${S_SUBNET%/*}
    net=${net%.*}
    env_set ROMULUS_CADDY_IP "$net.10"
    if [ "$S_CADDY" = yes ]; then
        env_set ROMULUS_PROXIES "$net.10/32"
    elif is_loopback_bind "$S_BIND"; then
        env_set ROMULUS_PROXIES "$net.1/32"
    else
        env_set ROMULUS_PROXIES ""
    fi
    env_set COMPOSE_PROFILES "$profiles"
    [ -z "$S_PROJECT" ] || env_set COMPOSE_PROJECT_NAME "$S_PROJECT"
    env_set ROMULUS_ADMIN_EMAIL "$S_EMAIL"
    env_set ROMULUS_ADMIN_NAME "$S_NAME"
    env_set ROMULUS_ORG_NAME "$S_ORG"
    env_set ROMULUS_ORG_SLUG "$S_SLUG"
    env_set ROMULUS_SIGNUP "$S_SIGNUP"
    env_set ROMULUS_SMTP_HOST "$S_SMTP_HOST"
    env_set ROMULUS_SMTP_PORT "$S_SMTP_PORT"
    env_set ROMULUS_SMTP_TLS "$S_SMTP_TLS"
    env_set ROMULUS_SMTP_USERNAME "$S_SMTP_USERNAME"
    env_set ROMULUS_SMTP_PASSWORD "$S_SMTP_PASSWORD"
    env_set ROMULUS_MAIL_FROM "$S_MAIL_FROM"
    if [ "$S_PLAYGROUNDS" = yes ]; then
        env_set ROMULUS_PLAY_URL http://playground:8080
    else
        env_set ROMULUS_PLAY_URL ""
    fi
    env_set ROMULUS_PLAY_DIR "$S_PLAY_DIR"
    [ -n "$(env_get ROMULUS_BACKUP_DIR)" ] || env_set ROMULUS_BACKUP_DIR ./backups
    [ -n "$(env_get ROMULUS_UID)" ] || env_set ROMULUS_UID "$(id -u)"
    [ -n "$(env_get ROMULUS_GID)" ] || env_set ROMULUS_GID "$(id -g)"
    for key in POSTGRES_PASSWORD SANDBOX_PASSWORD ROMULUS_PLAY_KEY ROMULUS_PLAY_SECRET; do
        [ -n "$(env_get "$key")" ] || env_set "$key" "$(random_hex 32)"
    done
    chmod 600 "$ENV_FILE"
}

LINK=""
# romulus-api setup: the admin, the organization, a one-time link (stdout).
setup_admin() {
    local email name org slug out rc=0
    email=$(env_get ROMULUS_ADMIN_EMAIL)
    name=$(env_get ROMULUS_ADMIN_NAME)
    org=$(env_get ROMULUS_ORG_NAME)
    slug=$(env_get ROMULUS_ORG_SLUG)
    if [ -n "$slug" ]; then
        out=$(compose exec -T api romulus-api setup --email "$email" --name "$name" --org "$org" --slug "$slug") || rc=$?
    else
        out=$(compose exec -T api romulus-api setup --email "$email" --name "$name" --org "$org") || rc=$?
    fi
    case $rc in
    0) LINK=$(printf '%s\n' "$out" | grep -v '^[[:space:]]*$' | tail -n 1 || true) ;;
    2)
        warn "this install already has an organization with another admin than $email: nothing created."
        warn "an existing account gets a password link with ./install.sh admin-link EMAIL"
        ;;
    *) die "romulus-api setup failed (exit $rc): docker compose logs api" ;;
    esac
}

mail_test() {
    local email=$1 rc=0
    say "Sending a test email to $email…"
    compose exec -T api romulus-api mail-test "$email" || rc=$?
    case $rc in
    0) say "Sent: check $email's inbox (and its spam folder)." ;;
    3) warn "mail is off (no SMTP host)" ;;
    *) warn "the test email failed (above): check the SMTP settings in .env, then ./install.sh again" ;;
    esac
}

cmd_install() {
    parse_install_options "$@"
    if interactive && [ ! -t 0 ]; then
        die "stdin is not a terminal: run with --non-interactive and options (./install.sh --help)"
    fi
    check_host
    # Volumes outlive .env: new passwords would lock the app out of its own data.
    if [ ! -f "$ENV_FILE" ]; then
        local project
        project=$(pick "$OPT_PROJECT" COMPOSE_PROJECT_NAME)
        project=${project:-romulus}
        if docker volume inspect "${project}_postgres-data" >/dev/null 2>&1; then
            die "the volume ${project}_postgres-data exists but .env is gone: put back .env from your backups (its passwords open that data), or, to start over, delete them: docker volume rm \$(docker volume ls -q --filter label=com.docker.compose.project=$project)"
        fi
    fi
    resolve_settings


    check_resources "$S_MAJORS" "$S_PLAYGROUNDS" "$S_CADDY"
    check_ports "$S_CADDY"

    if interactive; then
        say ""
        say "Ready to install romulus $S_VERSION:"
        say "  URL            $S_URL"
        say "  admin          $S_NAME <$S_EMAIL>, organization \"$S_ORG\""
        say "  HTTPS          $([ "$S_CADDY" = yes ] && echo "bundled Caddy" || echo "your own proxy, or none")"
        say "  Postgres       17${S_MAJORS:+, ${S_MAJORS// /, }}"
        say "  mail           ${S_SMTP_HOST:-off}"
        say "  playgrounds    $S_PLAYGROUNDS"
        local go_on
        ask_yn go_on "Go ahead?" yes
        [ "$go_on" = yes ] || { say "Stopped: nothing changed."; exit 1; }
    fi

    write_env
    (umask 077 && mkdir -p "$(backup_dir)")
    say "Settings in $ENV_FILE (mode 600)."

    if [ -z "$NO_PULL" ]; then
        say "Pulling the images…"
        pull_images || die "could not pull the images (version $(env_get ROMULUS_VERSION) from $(env_get ROMULUS_REGISTRY))"
    elif [ -n "$(env_get ROMULUS_PLAY_URL)" ] &&
        ! docker image inspect "$(env_get ROMULUS_REGISTRY)/romulus-playground:$(env_get ROMULUS_VERSION)" >/dev/null 2>&1; then
        warn "--no-pull: the playground image is not on this machine; playgrounds cannot start"
    fi
    say "Starting…"
    start_stack || die "romulus did not start (above). Fix it, then run ./install.sh again."
    setup_admin

    if [ -n "$(env_get ROMULUS_SMTP_HOST)" ]; then
        local send=""
        [ -z "$OPT_MAIL_TEST" ] || send=yes
        interactive && [ -z "$send" ] && ask_yn send "Send a test email to $(env_get ROMULUS_ADMIN_EMAIL)?" yes
        [ "$send" != yes ] || mail_test "$(env_get ROMULUS_ADMIN_EMAIL)"
    fi
    summary
}

summary() {
    local url host
    url=$(env_get ROMULUS_PUBLIC_URL)
    host=$(url_host "$url")
    say ""
    say "romulus $(env_get ROMULUS_VERSION) is running: $url/app/"
    if [ -n "$LINK" ]; then
        say ""
        say "Set $(env_get ROMULUS_ADMIN_EMAIL)'s password with this one-time link (valid 24 hours):"
        say ""
        say "  $LINK"
    else
        say "The admin is already set up. A forgotten password: ./install.sh admin-link EMAIL"
    fi
    say ""
    say "Next (docs/self-host/install.md):"
    if in_list caddy "$(env_get COMPOSE_PROFILES | tr ',' ' ')"; then
        say "  - DNS: point $host at this host; Caddy gets its certificate once it resolves."
    elif ! is_local_host "$host"; then
        say "  - TLS: proxy $url to http://$(env_get ROMULUS_BIND):$(env_get ROMULUS_PORT) (Caddyfile.example, or nginx in the docs)."
    fi
    if [ -z "$(env_get ROMULUS_SMTP_HOST)" ]; then
        say "  - Mail is off (recommended on: ./install.sh again): people locked out by wrong passwords"
        say "    cannot reset by email; invitation links are copied by hand; password links come from"
        say "    ./install.sh admin-link EMAIL."
    fi
    say "  - Backups: a dump every day in $(backup_dir). Copy them and .env off this host (docs/self-host/backup.md)."
    if [ -z "$(env_get ROMULUS_PLAY_URL)" ]; then
        say "  - Playgrounds are off: docs/self-host/playgrounds.md, then ./install.sh again."
    fi
    say "  - Status and logs: ./install.sh status, ./install.sh logs api"
}

# --- the other commands -----------------------------------------------------------

cmd_upgrade() {
    local version="" old dump
    while [ $# -gt 0 ]; do
        case $1 in
        --no-pull) NO_PULL=1 ;;
        -*) die "unknown option $1 (./install.sh --help)" ;;
        *)
            [ -z "$version" ] || die "upgrade takes one VERSION"
            version=$1
            ;;
        esac
        shift
    done
    require_env
    check_host
    old=$(env_get ROMULUS_VERSION)
    version=${version:-$ROMULUS_DEFAULT_VERSION}
    [[ $version =~ ^[A-Za-z0-9._-]+$ ]] || die "not a version: $version"
    say "Upgrading romulus $old → $version."
    ensure_postgres
    dump=$(backup_to "$(backup_dir)" "pre-upgrade-$old-to-$version") || die "the backup failed: nothing changed"
    say "Backed up to $dump"
    env_set ROMULUS_PREVIOUS_VERSION "$old"
    env_set ROMULUS_VERSION "$version"
    if ! pull_images; then
        env_set ROMULUS_VERSION "$old"
        die "could not pull $version: nothing changed (still $old)"
    fi
    if ! start_stack; then
        cat >&2 <<EOF

The upgrade to $version did not come up. Migrations only go forward: once
$version has migrated the database, $old refuses it. To go back to $old with
the database as it was before the upgrade (what was written since is lost
from it, but kept as the database romulus_before_restore):

  ./install.sh restore $dump --version $old

Or look first: ./install.sh logs api
EOF
        exit 1
    fi
    say "romulus is now $version (was $old). The pre-upgrade dump: $dump"
}

cmd_backup() {
    local dir=${1:-} path
    require_env
    dir=${dir:-$(backup_dir)}
    ensure_postgres
    path=$(backup_to "$dir" manual) || die "the backup failed"
    say "Wrote $path"
    say "and ${path%.dump}.env: it holds every secret of this install, keep it as safe as the dump."
}

cmd_restore() {
    local file="" version="" old pre
    while [ $# -gt 0 ]; do
        case $1 in
        --version)
            [ $# -ge 2 ] || die "--version needs a value"
            version=$2
            shift
            ;;
        --no-pull) NO_PULL=1 ;;
        -*) die "unknown option $1 (./install.sh --help)" ;;
        *)
            [ -z "$file" ] || die "restore takes one FILE"
            file=$1
            ;;
        esac
        shift
    done
    [ -n "$file" ] || die "usage: ./install.sh restore FILE [--version VERSION]"
    [ -f "$file" ] || die "no such file: $file"
    require_env
    check_host
    ensure_postgres
    compose exec -T postgres pg_restore --list <"$file" >/dev/null 2>&1 ||
        die "$file is not a dump this can restore (pg_dump --format=custom; pg_restore --list refuses it)"
    old=$(env_get ROMULUS_VERSION)
    if [ -n "$version" ] && [ "$version" != "$old" ]; then
        [[ $version =~ ^[A-Za-z0-9._-]+$ ]] || die "not a version: $version"
        env_set ROMULUS_VERSION "$version"
        if ! pull_images; then
            env_set ROMULUS_VERSION "$old"
            die "could not pull $version: nothing changed"
        fi
    fi
    revert() {
        env_set ROMULUS_VERSION "$old"
        compose up -d api >/dev/null 2>&1 || true
    }
    pre=$(backup_to "$(backup_dir)" pre-restore) || { revert; die "could not dump the current database first: nothing changed"; }
    say "The current database is saved as $pre"
    compose stop api
    if ! restore_db <"$file"; then
        revert
        die "the restore failed (above): the database is unchanged, the API restarted"
    fi
    [ "$version" = "" ] || [ "$version" = "$old" ] || env_set ROMULUS_PREVIOUS_VERSION "$old"
    start_stack || die "restored, but the API does not start (above): a dump newer than version $(env_get ROMULUS_VERSION) needs that version (--version)"
    say "Restored $file. The replaced database is kept as romulus_before_restore."
}

cmd_status() {
    require_env
    say "romulus $(env_get ROMULUS_VERSION) at $(env_get ROMULUS_PUBLIC_URL)/app/ (profiles: $(env_get COMPOSE_PROFILES | sed 's/^$/none/'))"
    compose ps --all
    local body
    if body=$(health_body); then
        say "health: $body"
    else
        warn "/api/health does not answer on $(health_url)"
        return 1
    fi
}

cmd_logs() {
    require_env
    compose logs --tail "${TAIL:-200}" --follow "$@"
}

cmd_admin_link() {
    [ $# -eq 1 ] || die "usage: ./install.sh admin-link EMAIL"
    require_env
    compose exec -T api romulus-api password-link "$1"
}

cmd_uninstall() {
    local delete="" confirm="" project answer scope ids play_url play_dir backups
    while [ $# -gt 0 ]; do
        case $1 in
        --delete-data) delete=1 ;;
        --confirm)
            [ $# -ge 2 ] || die "--confirm needs a value"
            confirm=$2
            shift
            ;;
        *) die "unknown option $1 (./install.sh --help)" ;;
        esac
        shift
    done
    require_env
    project=$(project_name)
    local all=()
    for profile in $ALL_PROFILES; do all+=(--profile "$profile"); done
    if [ -n "$delete" ]; then
        if [ -z "$confirm" ]; then
            [ -t 0 ] || die "--delete-data without a terminal needs --confirm $project"
            say "This deletes every volume of \"$project\": the database (every version, draft and"
            say "comment), the sandboxes, Caddy's certificates. Backups in $(backup_dir) stay."
            ask answer "Type $project to delete it all"
            confirm=$answer
        fi
        [ "$confirm" = "$project" ] || die "not confirmed: nothing deleted"
    fi
    scope=$(env_get ROMULUS_PLAY_SCOPE)
    play_url=$(env_get ROMULUS_PLAY_URL)
    play_dir=$(env_get ROMULUS_PLAY_DIR)
    backups=$(backup_dir)
    ids=$(docker ps -aq --filter "label=romulus.play.scope=${scope:-self-host}" 2>/dev/null || true)
    if [ -n "$delete" ]; then
        compose "${all[@]}" down --volumes --remove-orphans
        # shellcheck disable=SC2086 # one id per word
        [ -z "$ids" ] || docker rm -f $ids >/dev/null
        rm -f "$ENV_FILE"
        say "Deleted: containers, volumes and .env. Backups in $backups stay."
        if [ -n "$play_url$ids" ]; then
            say "Playground files stay in ${play_dir:-$DEFAULT_PLAY_DIR}; remove them as root if you want."
        fi
    else
        compose "${all[@]}" down --remove-orphans
        # shellcheck disable=SC2086 # one id per word
        [ -z "$ids" ] || docker stop $ids >/dev/null
        say "Stopped. The data stays in the volumes, with .env: ./install.sh starts it again."
    fi
}

usage() {
    sed -n '2,/^set -euo pipefail$/p' "$HERE/install.sh" | sed -e '$d' -e 's/^# \{0,1\}//'
}

command=${1:-install}
case $command in
install | upgrade | backup | restore | status | logs | admin-link | uninstall) shift || true ;;
-h | --help | help) usage && exit 0 ;;
-*) command=install ;;
*) die "unknown command $command (./install.sh --help)" ;;
esac
case $command in
install) cmd_install "$@" ;;
upgrade) cmd_upgrade "$@" ;;
backup) cmd_backup "$@" ;;
restore) cmd_restore "$@" ;;
status) cmd_status ;;
logs) cmd_logs "$@" ;;
admin-link) cmd_admin_link "$@" ;;
uninstall) cmd_uninstall "$@" ;;
esac
