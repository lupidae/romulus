# Releasing romulus

A release is a tag `vX.Y.Z` on main. Pushing it publishes everything: the
images (the Images workflow) and the Helm chart (the Helm workflow). One
version names both: the chart's version is the release's.

## Steps

1. **Pick the version.** `X.Y.Z`, semantic: a new `Y` for features, a new
   `Z` for fixes only. A pre-release is `vX.Y.Z-rc.N`.
2. **Bump the chart** in a pull request, then merge it to main:
   `deploy/helm/romulus/Chart.yaml`, `version: X.Y.Z` and
   `appVersion: "X.Y.Z"`. They must agree (`deploy/helm/test/lint.sh`
   checks it), and the tag must match them (the publish job checks it).
3. **Tag main and push the tag**, once main's CI is green:

   ```bash
   git fetch origin
   git tag -a vX.Y.Z -m "romulus X.Y.Z" origin/main
   git push origin vX.Y.Z
   ```

4. **Write the release notes** on GitHub (self-hosters are told to read
   them before upgrading, docs/self-host/upgrade.md): what changed, the
   database migrations and how long they may take, anything to do by hand,
   security fixes.

   ```bash
   gh release create vX.Y.Z --verify-tag --generate-notes --edit
   ```

5. **Check what was published** (below), from a machine signed in to
   nothing.

## What the tag publishes

- **Images** (`.github/workflows/images.yml`), to `ghcr.io/lupidae`:
  `romulus-api`, `romulus-sandbox` (and its `-pg15`, `-pg16`, `-pg18`
  variants), `romulus-play`, `romulus-playground`, each as:
  - `X.Y.Z`;
  - `X.Y`, when `X.Y.Z` is the newest release of `X.Y`;
  - `latest`, when it is the newest release of all. A fix released for an
    older line never moves `latest` back;
  - the commit's sha, as for every build.

  A pre-release gets its own tag only (`X.Y.Z-rc.N`). install.sh installs
  `latest` by default, so self-hosters get the new release on their next
  `./install.sh upgrade`.
- **The chart** (`.github/workflows/helm.yml`, job `publish`): lint; then
  it checks that Chart.yaml says `X.Y.Z`, waits (up to 2 hours) for the
  Images workflow to publish `X.Y.Z` of romulus-api and romulus-sandbox
  built from the tag's commit, and pushes
  `oci://ghcr.io/lupidae/charts/romulus`, version `X.Y.Z`. If the Images
  run fails, the publish stops too: fix it, then re-run both runs' failed
  jobs on the same tag.

Check:

```bash
docker buildx imagetools inspect ghcr.io/lupidae/romulus-api:X.Y.Z
docker buildx imagetools inspect ghcr.io/lupidae/romulus-sandbox:latest-pg18
helm show chart oci://ghcr.io/lupidae/charts/romulus --version X.Y.Z
```

## The first release only

GitHub creates the chart's package private. Make it public once: the
organization's **Packages**, `charts/romulus`, **Package settings**,
**Change visibility**, Public. While there, check that it is linked to
this repository. The image packages are public already; a new image
(one added to images.yml) needs the same once.

## When something goes wrong

- **The tag is on the wrong commit, nothing published yet**: delete it
  (`git push --delete origin vX.Y.Z` and `git tag -d vX.Y.Z`), fix, tag
  again.
- **Something was published**: never publish a version twice. Release
  `X.Y.(Z+1)` with the fix. People who pinned `X.Y.Z` must get what
  `X.Y.Z` was.
- **Before tagging, to try the chart on main's images**: run the Helm
  workflow by hand (Actions, Helm, Run workflow, `e2e` checked). It installs,
  upgrades, restores and uninstalls the chart on a throwaway k3d cluster.
