# Releasing romulus

A release is a tag `vX.Y.Z` on main. Pushing it publishes everything: the
images (the Images workflow), the Helm chart (the Helm workflow) and the
`romulus` command line (the CLI workflow). One version names them all: the
chart's version is the release's.

## Steps

1. **Pick the version.** `X.Y.Z`, semantic: a new `Y` for features, a new
   `Z` for fixes only. A pre-release is `vX.Y.Z-rc.N`.
2. **Bump the chart** in a pull request, then merge it to main:
   `deploy/helm/romulus/Chart.yaml`, `version: X.Y.Z` and
   `appVersion: "X.Y.Z"`. They must agree (`deploy/helm/test/lint.sh`
   checks it), and the tag must match them (the publish job checks it).
   Bump `server.json`'s `version` (the MCP Registry's entry) in the same
   pull request: a unit test in `mcp::protocol` refuses them apart.
3. **Tag main and push the tag**, once main's CI is green:

   ```bash
   git fetch origin
   git tag -a vX.Y.Z -m "romulus X.Y.Z" origin/main
   git push origin vX.Y.Z
   ```

4. **Write the release notes** on GitHub (self-hosters are told to read
   them before upgrading, docs/self-host/upgrade.md): what changed, the
   database migrations and how long they may take, anything to do by hand,
   security fixes. The CLI workflow creates the release when it attaches
   its binaries, if it does not exist yet: create it first, or edit it.

   ```bash
   gh release create vX.Y.Z --verify-tag --generate-notes --edit
   # or, once the CLI workflow made it:
   gh release edit vX.Y.Z --notes-file notes.md
   ```

5. **Check what was published** (below), from a machine signed in to
   nothing.

## What the tag publishes

- **Images** (`.github/workflows/images.yml`), to `ghcr.io/lupidae`:
  `romulus-api`, `romulus-sandbox` (and its `-pg15`, `-pg16`, `-pg18`
  variants), `romulus-play`, `romulus-playground`, each built for
  linux/amd64 and linux/arm64 (natively, one runner per architecture) and
  tagged as one multi-arch list, as:
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
  built from the tag's commit, both architectures, and pushes
  `oci://ghcr.io/lupidae/charts/romulus`, version `X.Y.Z`. If the Images
  run fails, the publish stops too: fix it, then re-run both runs' failed
  jobs on the same tag.

- **The command line** (`.github/workflows/cli.yml`, job `release`):
  `romulus-x86_64-unknown-linux-musl.tar.gz`,
  `romulus-aarch64-unknown-linux-musl.tar.gz`,
  `romulus-aarch64-apple-darwin.tar.gz` and `SHA256SUMS`, attached to the
  tag's GitHub release; `romulus --version` says `X.Y.Z`. The action
  (`action.yml`, `uses: lupidae/romulus@vX.Y.Z`) downloads the CLI of the
  tag it is pinned to, so a release must carry them.

- **The MCP Registry entry** is not published by the tag: once the
  release is out, publish `server.json` from the tag's checkout with
  [`mcp-publisher`](https://github.com/modelcontextprotocol/registry)
  (`mcp-publisher validate`, then `login` for the `com.lupidae`
  namespace and `publish`). A version is published once; the registry
  refuses it again.

Check (each lists linux/amd64 and linux/arm64):

```bash
docker buildx imagetools inspect ghcr.io/lupidae/romulus-api:X.Y.Z
docker buildx imagetools inspect ghcr.io/lupidae/romulus-sandbox:latest-pg18
helm show chart oci://ghcr.io/lupidae/charts/romulus --version X.Y.Z
gh release view vX.Y.Z --json assets --jq '.assets[].name'
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
