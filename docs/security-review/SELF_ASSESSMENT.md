# Self-assessment

What the author found while preparing this package, and what was done about each.
It is given so the reviewer knows what has already been looked at, not as a
substitute for the review. No exploit of any of them was attempted; rating the one vulnerability
(F1) is left to the reviewer.

| Id | Finding | Severity | Status |
| --- | --- | --- | --- |
| F1 | Three RustSec advisories in the TLS trust chain; dependencies were not checked automatically | Vulnerability in a dependency (distrusted roots and intermediates could be accepted); not rated here | Fixed in `cc7ab1e` |
| F2 | Native runtime helpers dereferenced a raw `Ctx` pointer behind safe signatures | Hardening (only generated code called them; no reachable misuse found) | Fixed in `cc7ab1e` |
| F3 | `unsafe` blocks and functions without written justification were not refused by the build | Hardening | Fixed in this commit |
| F4 | GitHub Actions referenced by moving tags | Hardening | Fixed in `d5caa94` |
| F5 | SECURITY.md out of date | Documentation | Fixed in this commit |
| F6 | Release archives have no signature or build provenance | Hardening | Fixed in this commit, from the next release |
| F7 | Release archive and wheel built without `--locked` | Hardening | Fixed in this commit |
| F8 | `deny.toml` checked a platform that is not built (Windows) and called it a release target | Documentation | Fixed in this commit |
| F9 | Fuzzing was short and not repeated; no fuzz target for token verification or for native code | Assurance | Fixed in this commit: `jwt` and `engines` targets; every target run for 15 minutes; weekly in CI |
| F10 | Fuzzing ran without AddressSanitizer | Assurance | Fixed in this commit: AddressSanitizer runs, and weekly in CI |
| F11 | Documentation of the HTTP transport stale in places | Documentation | Fixed in this commit |

## F1. Advisories in the TLS trust chain

The first run of `cargo deny` and `cargo audit` found RUSTSEC-2026-0381, -0382 and
-0383 ([SUPPLY_CHAIN.md](SUPPLY_CHAIN.md) section 4). Each affected certificate
verification for outgoing TLS (HTTP, TCP/TLS, PostgreSQL, key discovery). Fixed by
upgrading to versions at least two weeks old; the checks now run on every push and
weekly, so a new advisory against a crate already in the build is found within a week.

## F2. Raw pointers behind safe signatures

The `rt_*` helpers that generated code calls took `*mut Ctx` but were declared as
safe functions, so safe Rust could have called one with any pointer. Nothing but
generated code called them, and generated code passes the right pointer (tested on
every runtime case), so no misuse was reachable; but the signature did not state
the contract. Now each is an `unsafe extern "C" fn` with a `# Safety` section, and
each pointer becomes a reference in one place ([UNSAFE.md](UNSAFE.md) section 2).

## F3. Unjustified `unsafe`

Some `unsafe` blocks had no `// SAFETY:` comment and some `unsafe fn`s no `# Safety`
section. All 162 items now have one, clippy's `undocumented_unsafe_blocks` and
`missing_safety_doc` are on for the library, and CI refuses a new one without it.
`scripts/unsafe_inventory.py` lists them.

## F4. Actions by tag

Every `uses:` named a tag (`@v4`), which the action's owner can move, in workflows
that publish to PyPI and npm. Each is now a full commit SHA with the version in a
comment.

## F5. SECURITY.md

It said "a tool may not listen for connections"; since listening shipped, a tool
may, with a `wait_ms` limit on each accept (MCP.md section 2). It did not mention
the HTTP transport or how severity is rated. Corrected, with the HTTP transport
declared in scope for reports and CVSS v4.0 as the severity method.

## F6. No release signatures or provenance

`install.sh` checks the archive's SHA-256 against a `.sha256` file from the same
release, which catches corruption, not a release replaced by someone with write access
to the repository. PyPI uploads use trusted publishing and npm packages carry
provenance, but the archives had neither. Now the release job attests every
archive, wheel and npm package (`actions/attest`), and `install.sh` checks the
attestation with `gh attestation verify` when the GitHub CLI is installed and signed
in, requiring this repository's release workflow, the version's tag and a
GitHub-hosted runner; an archive of a release after 0.26.0 that fails is refused
([SUPPLY_CHAIN.md](SUPPLY_CHAIN.md) section 6). What remains: users without the
GitHub CLI get the hash check only, unless they set `STAAL_REQUIRE_PROVENANCE=1`; and
the attestation step runs for the first time at the next release, so it has not yet
run in a real release.

## F7. Unlocked release builds

`scripts/package.sh` and `scripts/wheel.sh` ran `cargo build --release` without
`--locked`. With a committed, consistent `Cargo.lock` the result is the same, but a
lock file out of step with `Cargo.toml` would have been updated silently, resolving
versions nobody reviewed. Both now pass `--locked`, which fails instead.

## F8. `deny.toml` targets

It listed `x86_64-pc-windows-msvc` as a release target. Windows is not built or
supported. Removed, so the checks cover exactly the four release platforms.

## F9 and F10. Fuzzing

Before this preparation the five fuzz targets had been run briefly, without
AddressSanitizer, and none ran native code: `config` and `journal` run the
interpreter only (an earlier draft of this file said otherwise; checking the code
corrected it). Two targets were added. `jwt` fuzzes the token verifier and asserts
that only the test issuer's own valid tokens verify. `engines` runs a runtime case,
its config changed by the fuzzer, on both engines and asserts the same result and
log: it is the target that reaches the native engine's `unsafe` code, and it runs
with AddressSanitizer. Every target was run for 15 minutes, `config`, `journal` and
`engines` also with AddressSanitizer ([ASSURANCE.md](ASSURANCE.md) section 5), and
every target runs with it weekly in CI (`.github/workflows/fuzz.yml`), seeded from the
repository by `scripts/fuzz_seed.sh`.

Fuzzing found one fault, in a fuzz target rather than in Staal: seeded with the test
tokens, `jwt` stopped at its first input, a genuinely signed token that must verify,
because the target asserted that nothing verifies. The target now asserts the
stronger property above.
AddressSanitizer is an unstable compiler option; it is used with the release
toolchain (Rust 1.97.0) through `RUSTC_BOOTSTRAP=1` rather than a nightly
toolchain, so the code fuzzed is compiled by the compiler that builds releases.

## F11. HTTP transport documentation

MCP.md section 12's heading still said "design, not built"; its table of checks gave
`-32022` for a missing `MCP-Protocol-Version` header, where the code (and its test)
answers `-32020` before reading the body; DEFERRED.md described the transport as not
built. Corrected to match the code.

## Looked at, nothing found

- **`git` argument injection** (`src/packages.rs`): locations and commits are
  checked before any `git` call, `--` precedes locations, and protocols are limited
  (THREAT_MODEL.md T59). The user's own git configuration, including credential
  helpers, still applies, as for any `git` the user runs.
- **Subprocesses**: only `cc` (`staalc build`), `git` (packages) and the operator's
  notify command (cleared environment) are started.
- **Workflows**: no `pull_request_target`; event data reaches scripts through
  environment variables; least privilege per job.
- **Bindings**: the Python package declares every returned pointer `c_void_p` and
  frees it once; the Node.js addon frees each result once.
- **Python and Node.js packages** have no runtime dependencies; `npm audit` reports
  none.
