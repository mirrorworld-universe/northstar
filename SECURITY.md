# Security Policy

## Reporting a Vulnerability

**Do not open a public GitHub issue for security problems.**

To report a vulnerability in NorthStar, please use GitHub's
[Report a Vulnerability](https://github.com/mirrorworld-universe/northstar/security/advisories/new)
flow. Provide a clear title, a detailed description of the vulnerability, and a
proof-of-concept where possible. Speculative submissions without a
proof-of-concept may be closed without further investigation.

If you have not already, please **enable two-factor authentication** on your
GitHub account before submitting a report.

We aim to acknowledge new advisories within 72 hours.

> **TODO:** add a fallback contact email (e.g. `security@…`) for cases where
> the GitHub advisory flow is unavailable.

## Scope

In scope:
- Source code in this repository, including the validator, runtime, and
  NorthStar-specific extensions.

Out of scope:
- Third-party dependencies — please report those upstream.
- Bugs that require social engineering to exploit.
- Findings from automated scanners without a working proof-of-concept.
- Any system or service whose source code does not live in this repository.

## Disclosure Process

1. **Triage.** A maintainer accepts the report into a draft GitHub Security
   Advisory and assesses severity.
2. **Fix.** A patch is prepared in a private fork associated with the advisory.
   The reporter is invited to review where appropriate.
3. **Coordinated disclosure.** Once a fix is ready, validators are notified
   ahead of public disclosure so they can prepare to upgrade.
4. **Release.** The fix is merged to the main repository and a new release is
   published. Validators are asked to upgrade promptly.
