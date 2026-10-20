# romulus

Design, version and review your Postgres schema together: the API (serving
the web app at `/app`), a sandbox Postgres per major that SQL compiles on,
the app's database (bundled, or yours), a daily dump. The same images and
settings as the compose bundle, one organization per install.

```bash
helm install romulus oci://ghcr.io/lupidae/charts/romulus \
  -n romulus --create-namespace --set host=romulus.example.com
```

Then create the first admin, as the install's notes say:

```bash
kubectl -n romulus exec deploy/romulus-api -- romulus-api setup \
  --email ada@example.com --name "Ada Lovelace" --org "Acme"
```

The guide, with every value, TLS, an external Postgres, upgrades, backups
and why playgrounds are not available on Kubernetes yet:
https://github.com/lupidae/romulus/blob/main/docs/self-host/kubernetes.md

AGPL-3.0-only. Source: https://github.com/lupidae/romulus
