# Security review package

Everything a security reviewer needs to review Staal: what it is, what it must
protect, how it is attacked, what defends it and where that is tested, what is
known not to hold, and how to rebuild every piece of evidence.

## The reviewed commit

| | |
| --- | --- |
| Repository | https://github.com/laurells/staal |
| Version | 0.27.0 |
| Commit | the commit tagged both `v0.27.0` and `security-review-1` |
| CI | the `ci` and `supply-chain` workflow runs for that commit passed before it was tagged |

Every count, line number and test name in this package is of that commit. A later
commit may change them; review at the tag. The release's archives, wheels and npm
package are built from it, each with a build provenance attestation. The package is written by the
implementation's author; it is the starting point for an independent review, not
its result.

## Contents

Read in this order:

| Document | What it gives the reviewer |
| --- | --- |
| [SCOPE.md](SCOPE.md) | what is in and out of scope, priorities, the standard, and the questions where an outside eye is worth most |
| [ARCHITECTURE.md](ARCHITECTURE.md) | components, how a run works, entry points, trust boundaries B1 to B12, durable state |
| [THREAT_MODEL.md](THREAT_MODEL.md) | assets, actors, and 65 threats (STRIDE per boundary), each with its mitigation, code, tests and residual risk |
| [COMPLIANCE.md](COMPLIANCE.md) | MCP 2026-07-28, RFC 9068, 9728, 8414, 6750, 8707, 7515, HTTP and TLS requirements against the code; OWASP ASVS 5.0.0 chapter map |
| [UNSAFE.md](UNSAFE.md) | every `unsafe` item with its justification, and the native engine's contract |
| [SUPPLY_CHAIN.md](SUPPLY_CHAIN.md) | dependencies, policy, automated checks, CI and release, SBOM |
| [ASSURANCE.md](ASSURANCE.md) | tests, crash injection, independent references, deliberate faults, fuzzing, and what none of it shows |
| [LIMITATIONS.md](LIMITATIONS.md) | known limits and accepted risks |
| [SELF_ASSESSMENT.md](SELF_ASSESSMENT.md) | what the author found preparing this, and what was done |
| [BUILD.md](BUILD.md) | building, testing and reproducing every result here |
| [sbom/](sbom/) | CycloneDX 1.5 SBOMs, one per release platform |

The specifications the package refers to: [GUARANTEES.md](../GUARANTEES.md) (the
claims), [RUNTIME.md](../RUNTIME.md), [GUIDE.md](../GUIDE.md), [MCP.md](../MCP.md),
[PACKAGES.md](../PACKAGES.md), and [SECURITY.md](../../SECURITY.md) (disclosure).

## Reporting findings

Privately, through GitHub's private vulnerability reporting on the repository, as
[SECURITY.md](../../SECURITY.md) describes. Findings against the HTTP transport
decide whether it is released (MCP.md section 12.7).
