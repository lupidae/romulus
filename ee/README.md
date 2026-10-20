# ee/: paid features (open core)

This directory will hold romulus's paid features, under the license in
`ee/LICENSE` (source available, production use under a commercial agreement),
not the AGPL that covers the rest of the repository.

**It is empty for now.** Planned:

- SAML and SCIM single sign-on and provisioning.
- Audit-log export and long retention.
- Advanced approval rules.
- A license-key check for self-hosted enterprise installs.

Priority support is a paid feature too, but it is a service, not code.

## The boundary

- **The AGPL core never depends on `ee/`.** Building and running romulus
  without this directory gives the full open-source product. No crate or package outside `ee/` imports
  anything from it.
- **`ee/` builds on the core**, through the core's public interfaces: an
  enterprise crate depends on core crates, never the reverse.
- **Wiring is optional.** The core exposes extension points (a trait, a
  feature-gated module, a configuration seam); `ee/` code plugs into them.
  With `ee/` absent or unlicensed, the core runs with its defaults.
- **Files never move silently across the line.** Moving code from the core
  into `ee/` (or back) changes its license: it is a decision of the
  maintainers, made in a pull request that says so.
- **Contributions** to `ee/` are accepted under the same CLA as the rest
  (CLA.md).
