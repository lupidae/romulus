# Security policy

## Reporting a vulnerability

Write to **security@lupidae.com**. Do not open a public issue, pull request
or discussion for a vulnerability, and do not share it elsewhere before it
is fixed.

Include what you can of:

- what the vulnerability is and what an attacker gains (data read or changed,
  access to another organization, code execution, …);
- the version or commit, and where it runs (self-hosted, the hosted service);
- steps to reproduce, a proof of concept, the requests involved;
- whether it is already public or exploited, as far as you know;
- how you want to be credited, if at all.

Test only against your own install or accounts. Do not read, change or delete
other people's data, and stop as soon as you have shown the problem.

## What happens next

- **Acknowledgement within 3 business days**, with a first assessment.
- **A fix, by severity**, from the confirmed report:

  | Severity | Target |
  |---|---|
  | Critical (remote access to other organizations' data, code execution) | 7 days |
  | High | 30 days |
  | Medium | 60 days |
  | Low | the next release |

  We keep you informed along the way, and say so if a fix needs longer.
- **Coordinated disclosure.** We publish an advisory once a fix is released,
  crediting you unless you prefer not to be. The default embargo is 90 days
  from the report; we agree on another date with you when a fix needs it.

## Supported versions

Security fixes go to **the latest release** and to the `main` branch. Older
releases are not patched: upgrade to get a fix.

## Scope

In scope:

- the romulus application: the API, the web app, the collaboration server,
  the sandbox compiler, the playground service and runner, and the images
  built from this repository;
- the self-host bundle under `deploy/self-host` as shipped.

Out of scope:

- the infrastructure of lupidae's hosted preview beyond the application
  itself (its hosting and DNS);
- social engineering of maintainers, users or providers, and physical attacks;
- denial of service by volume (load or flood tests); report a request that
  is unexpectedly expensive instead;
- issues in dependencies that do not affect romulus (report them upstream);
  in remus, report them to the remus project.
