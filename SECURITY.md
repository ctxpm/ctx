# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's
[private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
on this repository. Do not open a public issue.

Include what you found, how to reproduce it and which part is affected (security rules,
publish API, CLI, web UI).

## Trust model

- The Firebase web configuration and the OAuth client id of a registry are public by
  design. They grant nothing by themselves.
- Read access is enforced by the Firestore and Storage security rules in `registry/`.
- Write access to versions and artifacts is only possible through the publish API in
  `registry/functions`, which verifies the caller and makes published versions immutable.
- The CLI verifies the SHA-256 digest of every artifact against the registry index before
  it installs anything.
