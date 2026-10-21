# Run romulus on Kubernetes

The Helm chart in `deploy/helm/romulus` runs what the compose bundle
([install.md](install.md)) runs, with the same images, settings and
security posture: the API (serving the web app at `/app`), a sandbox
Postgres per major that SQL compiles on, the app's database (bundled, or
yours), a daily dump. One organization per install, password sign-in
built in, single sign-on optional.

Playgrounds are not supported on Kubernetes yet ([why](#playgrounds)).

## Requirements

- **Kubernetes 1.30 or later**, nodes on **x86-64 or arm64** (every image
  is published for both under the same tags; a node pulls its own, so a
  mixed cluster works too).
- **Helm 3.14 or later**, or Helm 4.
- **A default StorageClass** (or name one): the database and the dumps
  live on PersistentVolumeClaims, ReadWriteOnce.
- **About 3 GB of memory** for the base install's limits, 768 MB more per
  other Postgres major ([Sizing](#sizing)).
- **An ingress controller** and a DNS name, with a TLS certificate
  (cert-manager, or a Secret you made). Or only a Service, with your own way
  in.
- Optional: a network plugin that enforces NetworkPolicy (Calico, Cilium,
  the ones k3s and recent kind ship…). Without one, the chart's policies are
  accepted and do nothing.
- Optional: an **SMTP server**, for invitations, address checks and password
  resets by email.

## Quick start

```bash
helm install romulus oci://ghcr.io/lupidae/charts/romulus \
  -n romulus --create-namespace \
  --set host=romulus.example.com \
  --set ingress.tls.clusterIssuer=letsencrypt   # your cert-manager ClusterIssuer
```

Then follow what it prints (NOTES.txt):

```bash
kubectl -n romulus rollout status deploy/romulus-api --timeout=10m
kubectl -n romulus exec deploy/romulus-api -- curl -fsS http://127.0.0.1:8080/api/health
kubectl -n romulus exec deploy/romulus-api -- romulus-api setup \
  --email ada@example.com --name "Ada Lovelace" --org "Acme"
```

`setup` creates the first admin (email verified, no password yet) and the
organization, and prints a one-time link, valid 24 hours, that sets the
admin's password. Open it, choose the password, then invite your team from
the organization's members page. It is the command install.sh runs: run it
again for a fresh link while the password was never set; once it is set,
it does nothing. A fresh link for anyone, valid an hour:

```bash
kubectl -n romulus exec deploy/romulus-api -- romulus-api password-link ada@example.com
```

Every admin command of [configuration.md](configuration.md#admin-commands)
runs the same way (`unlock`, `auth-events`, `mail-test`…): `kubectl exec
deploy/romulus-api -- romulus-api <command>`.

Keep your values in a file rather than `--set` (`-f romulus.yaml`): every
`helm upgrade` needs them again.

### Versions

Releases are tagged `vX.Y.Z` on GitHub. The chart's version is the
release's, and it installs that release's images (`X.Y.Z`). Without
`--version`, Helm takes the newest; pin one:

```bash
helm install romulus oci://ghcr.io/lupidae/charts/romulus --version 0.1.0 -n romulus …
helm show chart oci://ghcr.io/lupidae/charts/romulus --version 0.1.0   # what it is
```

### A trial without an ingress

```bash
helm install romulus oci://ghcr.io/lupidae/charts/romulus -n romulus --create-namespace \
  --set publicUrl=http://localhost:8080 --set ingress.enabled=false
kubectl -n romulus port-forward svc/romulus-api 8080:8080
```

Then open http://localhost:8080/app/. An http URL is for a trial on
localhost only: session cookies are `Secure` only over https.

### From a clone

The chart in a release's tag installs that release. On main, it names the
last release's images (its appVersion) while the templates may be newer;
main's own images are tagged `main` and by commit:

```bash
git checkout v0.1.0
helm install romulus deploy/helm/romulus -n romulus --create-namespace --set host=romulus.example.com
# or main's images, for testing: --set image.tag=main (or a commit sha)
```

## What runs

| workload | what | limits (requests) |
|---|---|---|
| Deployment `romulus-api` | the API and the web app, one replica (Recreate) | 1 GiB (256 MiB, 0.1 CPU) |
| StatefulSet `romulus-postgresql` | the app database, `postgres:18.6`, 8 GiB volume | 768 MiB (256 MiB, 0.1 CPU) |
| StatefulSet `romulus-sandbox` | where SQL compiles, Postgres 17, 1 GiB volume | 768 MiB, 1 CPU (the same: Guaranteed) |
| StatefulSet `romulus-sandbox-15`, `-16`, `-18` | the other majors, when `sandbox.extraMajors` names them | the same each |
| CronJob `romulus-backup` | a dump every day at 03:00 UTC | 256 MiB (64 MiB) |
| Job `romulus-backup-pre-upgrade` | a dump before each upgrade (a hook) | the same |
| Secret `romulus` | the database and sandbox passwords, generated once | |

Names start with the release's (`romulus` above). The memory limits are
compose's; limits are caps, requests what the scheduler reserves.

The API is one replica by design: drafts' live documents and their sockets
live in its process, so two at once would split them. It is replaced with
`Recreate` (the old one writes every open draft and stops before the new one
starts) and never scaled.

### Sizing

The base install's limits add up to about 2.8 GiB (requests: about 1.3
GiB). Each other Postgres major adds 768 MiB, requested in full. Keep the
sandbox at 768 MiB or more, with requests equal to limits: its compile pool
is sized for it, and Guaranteed QoS is what protects its server from a
compile running out of memory ([Hardening](#hardening)).

## Values

`helm show values oci://ghcr.io/lupidae/charts/romulus` prints them all,
commented; `values.schema.json` refuses unknown keys and wrong types.

### Where it answers

| value | default | what |
|---|---|---|
| `host` | | the name people open romulus at: the Ingress's rule, and the public URL `https://<host>` |
| `publicUrl` | `https://<host>` | the browser's origin, no path. Every unsafe request must come with this `Origin` (CSRF), cookies are `Secure` when it is https, links in emails start with it |
| `ingress.enabled` | `true` | an Ingress for `host`, everything to the API |
| `ingress.className` | the cluster's default | the IngressClass |
| `ingress.tls.enabled` | `true` | TLS for `host` |
| `ingress.tls.secretName` | `<release>-tls` | the certificate's Secret: one you made, or the one cert-manager writes |
| `ingress.tls.clusterIssuer` | | a cert-manager ClusterIssuer (`cert-manager.io/cluster-issuer`) |
| `ingress.annotations` | ingress-nginx's body size (64m), timeouts (1 h) and buffering (off) | more annotations; set one to `null` to drop it |
| `api.service.type` | `ClusterIP` | `NodePort` or `LoadBalancer` only with TLS in front: the API speaks plain HTTP |
| `api.trustedProxies` | | whose `X-Forwarded-For` the API believes ([Client addresses](#client-addresses)) |

### Accounts and mail

| value | default | what |
|---|---|---|
| `auth.methods` | `password` | `password`, or `password,oidc` for single sign-on beside it. The first admin is a password account, so `password` stays |
| `auth.signup` | `invite` | `invite`: accounts only from invitation links. `open`: anyone who reaches the URL, proving the address by email (needs mail) |
| `auth.oidc.issuer`, `.clientId`, `.clientSecret` | | your OpenID Connect client ([configuration.md](configuration.md#sign-in-with-your-own-identity-provider)); redirect URI `<publicUrl>/api/auth/callback` |
| `auth.oidc.existingSecret`, `.existingSecretKey` | | the client secret from a Secret of yours instead |
| `smtp.host`, `.port`, `.tls`, `.username`, `.password`, `.from` | mail off | [configuration.md](configuration.md#mail-smtp) has provider examples |
| `smtp.existingSecret`, `.existingSecretKey` | | the SMTP password from a Secret of yours instead |
| `admin.email`, `.name`, `.organization`, `.slug` | | what NOTES.txt puts in the `setup` command |
| `admin.setupJob` | `false` | runs `setup` in a Job after each install and upgrade. Its log holds the link: anyone who can read the namespace's logs, or your log collector, sees it. `kubectl logs job/romulus-setup` |

The mail test, once running:

```bash
kubectl -n romulus exec deploy/romulus-api -- romulus-api mail-test you@example.com
```

### Images and secrets

| value | default | what |
|---|---|---|
| `image.registry` | `ghcr.io/lupidae` | where `romulus-api` and `romulus-sandbox` come from |
| `image.tag` | the chart's appVersion | the images' tag: `X.Y.Z`, `X.Y`, `latest`, `main` or a commit sha; a chart version installs its own release's |
| `imagePullSecrets` | | for a private registry |
| `existingSecret` | | a Secret with `postgres-password` and `sandbox-password` (16 characters or more each) instead of generated ones |

Without `existingSecret`, the chart generates both passwords (64 random
characters) on the first install, reads them back from the cluster on every
upgrade, and never replaces them; the Secret is kept when the release is
uninstalled (`helm.sh/resource-policy: keep`), like the volumes its
passwords open. If the Secret is gone but the volumes are not, the chart
refuses to install new passwords over them (install.sh does the same when
`.env` is gone): put the Secret back from your backups, or delete the
volumes to start over. No value ever defaults to a password, and romulus's
development passwords are refused.

To read them: `kubectl -n romulus get secret romulus -o jsonpath='{.data.postgres-password}' | base64 -d`.

### Postgres majors, resources, placement

| value | default | what |
|---|---|---|
| `sandbox.extraMajors` | `[]` | other majors a database may target: any of `15`, `16`, `18` (17 always runs), e.g. `--set 'sandbox.extraMajors={15,18}'` |
| `sandbox.resources` | 768 MiB, 1 CPU | keep requests equal to limits |
| `sandbox.maxDataKb` | `131072` | each sandbox Postgres process's private memory cap, KiB ([deploy/sandbox/README.md](../../deploy/sandbox/README.md)); raise it only with the memory limit |
| `sandbox.persistence` | 1 GiB | the compile pool's roles and databases. Off: an emptyDir, and the API must be restarted after a sandbox pod moves |
| `postgresql.persistence.size`, `.storageClass` | 8 GiB, the default class | the app database's volume |
| `postgresql.settings` | compose's | `-c` arguments: `shared_buffers`, `effective_cache_size`, `max_connections` |
| `api.resources`, `postgresql.resources`, `backup.resources` | compose's limits | |
| `*.nodeSelector`, `*.tolerations`, `*.affinity` | | for `api`, `postgresql`, `sandbox` |
| `api.logLevel` | `romulus_api=info,tower_http=info,sqlx=warn` | `RUST_LOG` |
| `api.extraEnv` | | more environment for the API ([configuration.md](configuration.md#the-api-romulus-api) lists what it reads) |

### Network policies, budgets

| value | default | what |
|---|---|---|
| `networkPolicy.enabled` | `true` | only the API reaches the sandboxes; only the API and the backup and setup jobs reach Postgres; neither Postgres nor the sandboxes connect anywhere |
| `networkPolicy.apiAllowFrom` | anyone | NetworkPolicy peers allowed to the API's port, e.g. your ingress controller's namespace |
| `podDisruptionBudget.enabled` | `false` | `maxUnavailable: 0` for the API and Postgres ([below](#disruptions)) |

## External Postgres

A database you run (a managed Postgres, an operator's cluster), Postgres 15
or later: the bundled StatefulSet is not created.

```yaml
postgresql:
  external:
    enabled: true
    host: romulus.abc123.eu-west-1.rds.amazonaws.com
    port: 5432
    database: romulus
    user: romulus
    sslmode: verify-full      # disable, prefer, require (default), verify-ca, verify-full
    password: "…"             # stored in the chart's romulus-env Secret, URL-encoded
```

Or from a Secret you made:

```yaml
postgresql:
  external:
    enabled: true
    existingSecret: romulus-db
    existingSecretUrlKey: uri          # the whole URL: postgres://user:password@host:5432/romulus?sslmode=require
```

```yaml
postgresql:
  external:
    enabled: true
    host: pg-rw.databases.svc
    existingSecret: pg-app             # e.g. what an operator writes
    existingSecretPasswordKey: password   # URL-safe characters only, or use the URL form
```

The user must own the database (the API creates its tables and applies its
migrations when it starts). `verify-full` checks the server's certificate
against the system roots. With a managed database you may prefer its own
backups: `backup.enabled=false`, or keep both.

## TLS

- **cert-manager**: `ingress.tls.clusterIssuer=<your ClusterIssuer>`; it
  writes the certificate into `ingress.tls.secretName` (`<release>-tls`)
  and renews it.
- **A certificate you have**: create the Secret
  (`kubectl -n romulus create secret tls romulus-tls --cert=… --key=…`)
  and keep `ingress.tls.secretName` pointing at it.
- **TLS terminated before the cluster** (a cloud load balancer):
  `ingress.tls.enabled=false`; `publicUrl` stays https.

Send HSTS from the ingress controller (ingress-nginx does by default on TLS
hosts). The defaults' annotations are ingress-nginx's: imports up to 64 MB,
websockets (live drafts) kept open for an hour, import progress streamed.
With another controller (Traefik, HAProxy, a Gateway), give it the same:
no body limit under 64 MB, no idle timeout under a few minutes on
websockets, no response buffering on `/api`.

With the Gateway API instead of an Ingress, set `ingress.enabled=false` and
route your hostname to the Service:

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: romulus
  namespace: romulus
spec:
  parentRefs: [{name: my-gateway, namespace: gateways}]
  hostnames: [romulus.example.com]
  rules:
    - backendRefs: [{name: romulus-api, port: 8080}]
```

## Client addresses

romulus limits sign-in attempts and other requests per client address, and
logs it with sign-in events. Behind an ingress controller every request
comes from the controller's pod, so the API reads the client's address from
`X-Forwarded-For`, but only from the addresses in `api.trustedProxies`.
Empty (the default), the header is ignored and every visitor counts as the
controller: per-client limits act as one global limit, which one client
guessing passwords can fill for everyone.

Set it to the addresses your controller's pods connect from (usually the
cluster's pod network, e.g. `10.42.0.0/16` on k3s, `10.244.0.0/16` on kind
and kubeadm defaults), and let only the controller reach the API's port, so
that no other pod can claim an address:

```yaml
api:
  trustedProxies: 10.244.0.0/16
networkPolicy:
  apiAllowFrom:
    - namespaceSelector:
        matchLabels:
          kubernetes.io/metadata.name: ingress-nginx
```

The controller must see the client's real address itself (a cloud load
balancer in front: `externalTrafficPolicy: Local` on its Service, or the
PROXY protocol). The API takes the right-most address of `X-Forwarded-For`
that is not a trusted proxy, so a controller that appends to the header is
fine. Never trust a network clients are in, or `0.0.0.0/0` (refused).

## Migrations

The API applies pending database migrations when it starts, as it does in
the compose bundle; the chart does not run them in a hook Job. Why:

- **One API at a time.** The API is one replica, replaced with `Recreate`:
  the old process is gone before the new one migrates, so there is no
  window where an old API runs on a migrated schema. sqlx's migrator also
  takes an advisory lock, so two migrators (the API and an admin command)
  never interleave.
- **A pre-install hook runs before anything else exists**: the bundled
  database and the generated Secret would have to become hooks too, which
  takes them out of the release (Helm does not upgrade or track hook
  resources).
- **A pre-upgrade hook would migrate while the old API still runs**, and a
  failed upgrade would leave the old API on a schema it refuses: romulus's
  migrations only go forward, and an older API refuses a database a newer
  one migrated. Rolling back takes a restore either way ([Upgrade](#upgrade)).
- **The startup probe waits up to 5 minutes** for a long migration (compose's
  `start_period`), then liveness and readiness check `/api/health`.

The pre-upgrade hook the chart does run is the dump, so that going back is
always possible.

## Upgrade

1. Read the release notes.
2. Upgrade with the same values:

   ```bash
   helm upgrade romulus oci://ghcr.io/lupidae/charts/romulus -n romulus -f romulus.yaml --version <new>
   ```

What happens:

1. The pre-upgrade hook dumps the database into the backups volume
   (`romulus-<time>-pre-upgrade-<old>-to-<new>.dump`). If the dump fails,
   the upgrade stops before anything changes.
2. The API stops (it writes every open draft first; editors reconnect on
   their own), the new one starts and applies the new migrations. The
   sandboxes restart with their new image; Postgres only when its own
   settings change.
3. Secrets are read back from the cluster: an upgrade never changes them.

Changing a value is the same command with the new value.

**Going back.** `helm rollback` alone is not enough once the new API has
migrated the database: the older API refuses it. Roll back, then restore
the pre-upgrade dump ([Restore](#restore)):

```bash
helm rollback romulus -n romulus
kubectl -n romulus scale deploy/romulus-api --replicas=0
kubectl -n romulus exec romulus-postgresql-0 -- bash /scripts/restore.sh /backups/romulus-<time>-pre-upgrade-<old>-to-<new>.dump
kubectl -n romulus scale deploy/romulus-api --replicas=1
```

`postgres:18.6` is pinned in the chart; a release that moves it says so.
Moving the app database to another Postgres major is a dump and a restore.

## Backups

What to keep, as with compose ([backup.md](backup.md)): the app database,
and the Secret `romulus` (its passwords open the volumes; without it, a
dump restores into any install anyway). Not worth keeping: the sandboxes'
volumes.

The CronJob `romulus-backup` dumps the database every day at
`backup.schedule` (`0 3 * * *`) in `backup.timeZone` (UTC) into the volume
`romulus-backups`, with `deploy/self-host/backup.sh`: `pg_dump
--format=custom`, written beside, checked to read back, then renamed. Daily
dumps older than `backup.keepDays` (14) go once a new one is written;
pre-upgrade, pre-restore and manual dumps are never deleted. With the
bundled database the dumps are also mounted in its pod at `/backups`, and
the jobs run beside it (the volume is ReadWriteOnce).

| value | default | what |
|---|---|---|
| `backup.enabled` | `true` | the CronJob, the volume, the pre-upgrade dump |
| `backup.schedule`, `backup.timeZone` | `0 3 * * *`, `Etc/UTC` | |
| `backup.keepDays` | `14` | |
| `backup.beforeUpgrade` | `true` | the pre-upgrade hook |
| `backup.persistence.size`, `.storageClass`, `.existingClaim` | 8 GiB | |
| `backup.image` | `postgresql.image` | `pg_dump`'s image (18 dumps 15 to 18) |

A dump now, and the list:

```bash
kubectl -n romulus create job --from=cronjob/romulus-backup backup-now
kubectl -n romulus exec romulus-postgresql-0 -- ls -l /backups
```

**Copy them off the cluster**: dumps on the same storage as the database
are not a backup. For example, every day:

```bash
kubectl -n romulus cp romulus-postgresql-0:/backups/romulus-<time>-daily.dump ./romulus-<time>-daily.dump
```

then to your backup storage (restic, rclone, …), with a copy of the Secret
(`kubectl -n romulus get secret romulus -o yaml`), encrypted. Volume
snapshots (Velero, your storage's own) of `romulus-backups` work too.

With an external database, the dumps are only in the volume: read them from
a pod that mounts it, or rely on the database's own backups.

### Restore

`restore.sh` (in the chart, mounted at `/scripts` in the bundled database's
pod) does what `install.sh restore` does: checks the dump reads, dumps the
current database first (`pre-restore`), restores into a new database, then
swaps it in by renaming, so a failure before the swap changes nothing. The
replaced database stays as `romulus_before_restore`. It refuses while the
API is connected.

```bash
kubectl -n romulus scale deploy/romulus-api --replicas=0
# a dump in the volume
kubectl -n romulus exec romulus-postgresql-0 -- bash /scripts/restore.sh /backups/romulus-<time>-daily.dump
# or one from your machine
kubectl -n romulus exec -i romulus-postgresql-0 -- bash /scripts/restore.sh - < romulus-<time>-daily.dump
kubectl -n romulus scale deploy/romulus-api --replicas=1
```

A dump from an older version restores as is: the API migrates it forward
when it starts. A dump from a newer version needs that version
(`--set image.tag=…`). Once happy with the result:

```bash
kubectl -n romulus exec romulus-postgresql-0 -- dropdb -U romulus romulus_before_restore
```

With an external database, restore with `pg_restore --no-owner
--single-transaction` into an empty database, the API stopped.

## Hardening

What the chart does, and what is left to you ([hardening.md](hardening.md)
covers the rest: accounts, mail, logs, what romulus does not do yet).

- **Pod Security "restricted"** for every pod: not root (the API as 10001,
  Postgres as 999), no privilege escalation, every capability dropped, the
  runtime's seccomp profile, a read-only root filesystem (the writable
  paths are emptyDirs). Label the namespace to enforce it:
  `kubectl label ns romulus pod-security.kubernetes.io/enforce=restricted`.
- **No Kubernetes API access**: a ServiceAccount without a token mounted,
  no Role.
- **NetworkPolicies** (on by default): only the API reaches the sandboxes
  and only the API and the backup and setup jobs reach Postgres; neither
  connects anywhere (no egress). Restrict the API's port to your ingress
  controller with `networkPolicy.apiAllowFrom`.
- **The sandbox**: compiles run SQL people wrote. Its image caps each
  Postgres process's private memory (128 MB), each backend sets its own OOM
  score to 1000, and Guaranteed QoS (requests equal to limits) gives the
  postmaster -997: a compile that runs out of memory fails or kills its
  backend, never the server (compose sets -1000 on the container). Keep its
  requests equal to its limits.
- **Secrets** generated once, never in values unless you put them there
  (then in the release's Secret `romulus-env`, and in Helm's release
  history: prefer `existingSecret` forms). Encrypt Secrets at rest in your
  cluster (etcd encryption), and limit who can read Secrets and pod logs in
  the namespace.
- **TLS** at the ingress; HSTS; `publicUrl` https.
- **Resources**: the compose bundle's memory limits; the API and Postgres
  without CPU limits, the sandboxes with one.

### Disruptions

Every workload runs one replica, so a PodDisruptionBudget cannot keep it up
through a node drain; it can only make the drain wait. That is what
`podDisruptionBudget.enabled=true` does (`maxUnavailable: 0` for the API
and Postgres): `kubectl drain` waits until you delete the pod yourself, at a
moment you choose. Otherwise an eviction is an ordinary restart: the API
writes open drafts on its way out, Postgres shuts down cleanly, editors
reconnect.

## Playgrounds

**Not supported on Kubernetes yet.** The chart refuses
`playgrounds.enabled=true`, and the app says playgrounds are off. Run
romulus with the compose bundle ([playgrounds.md](playgrounds.md)) where
you need them.

Why: the playground runner (`romulus-play-runner`) creates each playground
as a Docker container, through Docker Engine's socket: with gVisor's
`runsc-uds` runtime, no network, a read-only root, an XFS project quota set
by a short-lived privileged `xfs_quota` container. Under Kubernetes:

- Cluster nodes run containerd or CRI-O, not Docker Engine. Installing
  Docker beside the kubelet on a node pool, and mounting its socket into the
  runner's pod, would run every playground outside Kubernetes: not
  scheduled, not counted in the node's allocatable memory (the kubelet
  evicts real pods to make room for containers it cannot see), not under
  Pod Security or quotas, not removed by `helm uninstall`.
- The runner's pod would need a hostPath to that socket, and the socket is
  root on the node. In the compose bundle, that is root on the one host
  romulus already owns; on a cluster node, it is a foothold into the
  cluster (the node's kubelet credentials, other pods' secrets on it), and
  the namespace would have to allow privileged pods.
- A gVisor `RuntimeClass` does not help on its own: the runner would have to
  create pods instead of containers, with per-playground volumes under XFS
  quotas and a way for the service to reach each playground's Postgres
  without a network. That is a new runner backend, not a chart setting.

None of these is acceptable for the isolation playgrounds need, so the chart
leaves them off rather than half-working. Native support (a Kubernetes
backend for the runner) is possible future work.

## Uninstall

```bash
helm uninstall romulus -n romulus
```

It removes the workloads, Services, Ingress, CronJob and policies, and
**keeps**:

- the volumes: `data-romulus-postgresql-0` (the database),
  `data-romulus-sandbox-0` (and `-15`, `-16`, `-18`), `romulus-backups`;
- the Secret `romulus` (the passwords that open them).

Installing again with the same release name in the same namespace picks
them all up: the same data, the same passwords. To delete everything, the
dumps included:

```bash
helm uninstall romulus -n romulus
kubectl -n romulus delete pvc -l app.kubernetes.io/instance=romulus
kubectl -n romulus delete secret romulus
kubectl delete namespace romulus
```

## When something is wrong

- `kubectl -n romulus get pods`, then `kubectl -n romulus describe pod …`
  and `kubectl -n romulus logs deploy/romulus-api` (JSON lines).
- `/api/health` says `database`, `sandbox` and `sandboxes` (`ok`,
  `unavailable` or `off`) and `playgrounds` (`off`):
  `kubectl -n romulus exec deploy/romulus-api -- curl -fsS http://127.0.0.1:8080/api/health`.
- **The API waits in `Init`**: its `wait-for-database` container waits for
  Postgres to accept connections (`kubectl logs … -c wait-for-database`).
- **A sandbox is `unavailable`**: uploads and drafts on that major answer
  503 until it answers. `kubectl -n romulus logs sts/romulus-sandbox`.
- **"the volume … exists but the Secret romulus is gone"**: see
  [Images and secrets](#images-and-secrets).
- **Pods Pending**: no default StorageClass (set the `storageClass`
  values), or not enough memory for the sandbox's Guaranteed request.
- **Sign-in rate limits hit everyone at once**: `api.trustedProxies`
  ([Client addresses](#client-addresses)).

## Testing the chart

`deploy/helm/test/lint.sh` renders every values file in
`deploy/helm/test/values` and validates it (helm lint, kubeconform against
the Kubernetes 1.30 and newest schemas, what the chart must refuse).
`deploy/helm/test/e2e.sh` installs the chart on a throwaway k3d cluster in a
namespace enforcing Pod Security "restricted", then exercises the first
admin, pushes on two majors, the network policies, backups, an upgrade, a
restore, an uninstall and a reinstall. CI runs the first on every pull
request touching `deploy/helm`; the second by hand (the Helm workflow).
