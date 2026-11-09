# Assurance evidence

What has been done to show the code does what the threat model says, how to rerun
it ([BUILD.md](BUILD.md)), and what it does not show. Counts are of the reviewed
commit.

## 1. Tests

All in `cargo test --features failpoints` unless said otherwise; CI runs them on
Linux x86_64 and arm64 and macOS on Apple Silicon and Intel, and again without the
native engine.

| Kind | Count | Where |
| --- | ---: | --- |
| Compiler fixtures: programs that must be refused, each with its error code and message | 628 | `tests/fixtures/fail` |
| Compiler fixtures: programs that must be accepted | 54 | `tests/fixtures/pass` |
| Runtime cases (program, run config, expected result), each run on both engines and compared | 424 | `tests/run` |
| Integration tests | 296, in 45 files | `tests/*_test.rs` |
| Unit tests | 155 | `src/` |
| Python package tests (run by `capi_test.rs`) | 5 files | `python/tests/` |
| Node.js package tests | 12 | `node/test/staal.test.mjs` |

Refused fixtures by rule: E001 19, E002 8, E003 21, E004 16, E005 3, E006 40, E007 99,
E008 16; the rest are type, name and syntax errors (E100 to E109, E900, E901).

The integration tests most relevant to the threat model, by area:

| Area | Files |
| --- | --- |
| Engines agree | `engines_test.rs` (every runtime case on both engines: results, logs and journals), `aot_test.rs` (every case as an executable), `split_test.rs` (every program split across modules gives the same results) |
| Crash safety | `crash_test.rs`, `http_crash_test.rs`, `library_crash_test.rs`, `mcp_crash_test.rs` |
| Capabilities | `http_test.rs`, `https_test.rs`, `tls_test.rs`, `tcp_test.rs`, `serve_test.rs`, `db_test.rs`, `postgres_test.rs`, `library_test.rs`, `parallel_test.rs`, `deadline_test.rs`, `ffi_test.rs` |
| Tool host and MCP | `mcp_test.rs`, `mcp_http_test.rs`, `mcp_keys_test.rs`, `mcp_sdk_test.rs`, `capi_test.rs` |
| Packages | `packages_test.rs`, `package_cases_test.rs`, `versions_test.rs` |
| Release | `install_test.rs`, `release_test.rs` |

## 2. Crash injection

A test build (`--features failpoints`) can stop the process at any journal write
(`STAALC_FAILPOINT`). The crash tests stop a run at every such point, resume it, and
check that it ends exactly as an uninterrupted run does, with every action taken once
(counted by the stand-in service), on every pairing of engines for the crash and the
resume. Covered: actions over HTTP (deduplicated and not), files, TCP, listening over
TCP and TLS, model calls, library capabilities (a Stripe refund and a Termii SMS),
C calls, concurrent tasks, the tool host's ledger and `once` keys, and the MCP server
over stdio and HTTP.

## 3. Independent references

Where a result can be computed by someone else's code, it is:

- **JWT verification**: 38 tokens made by PyJWT with `cryptography`
  (`tests/fixtures/jwt/`, made by `scripts/jwt_fixtures.py`), each with its expected
  verdict; and the RFC 7515 appendix A.3 and RFC 8037 appendix A.4 examples.
- **Principal scope**: HMAC-SHA-256 vectors computed with Python's `hmac`.
- **MCP**: the official Python and TypeScript SDK clients call tools over stdio and
  over HTTPS with OAuth `client_credentials` (`mcp_sdk_test.rs`, in CI).
- **TLS**: certificates from a test authority made with `rcgen`, checked by rustls as
  a client and, in the SDK test, by Python's `ssl` and Node.js.
- **PostgreSQL**: a real server, started by the tests.
- **Adversarial example**: `examples/finops/attacks.py` attacks a finance agent with
  prompt injection, unauthorised amounts, malicious arguments, replays, concurrency
  against limits, budget exhaustion, SIGKILL after the bank acted, lost answers and a
  mistaken policy; each judged by what the stand-in bank and SMS gateway received
  ([GUARANTEES.md](../GUARANTEES.md)); run on both engines by `capi_test.rs`.

## 4. Deliberate faults

Each feature was finished only when breaking it on purpose (removing a check,
inverting a condition, returning early) made a test fail (CONTRIBUTING.md). Recorded
results:

| Area | Faults, each caught | Record |
| --- | ---: | --- |
| HTTP and files | 17 | GUIDE.md "HTTP and files" |
| Library capabilities | 16 | GUIDE.md "Capabilities from libraries" |
| Packages | 17 | GUIDE.md "Packages" |
| Versions and registries | 12 | GUIDE.md "Packages" |
| C libraries | 18 | GUIDE.md "C libraries" |
| Native engine | 27 | GUIDE.md "Native code and the C interface" |
| Executables | 5 | GUIDE.md "Executables" |
| JWT verification (first version) | 5 | commit `ae41ef0` |
| MCP HTTP transport, tokens, keys, principals, limits | 39 | `scripts/mutants_http.py` (below) |

These were made by hand for each change; apart from the last row there is no script
to rerun them, so they are a record, not a suite. For the HTTP transport, which this
review is the gate for, the faults are in a script anyone can rerun
(`python3 scripts/mutants_http.py`). Its result at the reviewed commit:

39 of 39 faults caught, none failing to build. The tests run for each: the token
verifier's unit tests for J (token and key set checks), the key fetcher's unit tests
and `mcp_keys_test.rs` for K (discovery and key caching), and `mcp_http_test.rs` for
H (the request checks, tokens on the endpoint, scopes, principals, cancellation and
per-principal limits). The script first checks the tests pass with no fault.

| Id | Fault | | Id | Fault |
| --- | --- | --- | --- | --- |
| J1 | `typ` not checked | | H1 | `Host` not checked |
| J2 | `crit` allowed | | H2 | `Origin` not checked |
| J3 | a key's own `alg` not checked | | H3 | a request served without a valid token |
| J4 | RSA keys under 2048 bits allowed | | H4 | two `Authorization` headers allowed |
| J5 | signature not checked | | H5 | request rate not limited |
| J6 | `iss` not checked | | H6 | tool scopes not checked |
| J7 | `aud` not checked | | H7 | every subject one principal |
| J8 | `exp` not checked | | H8 | `Content-Type` not checked |
| J9 | `nbf` not checked | | H9 | `Accept` not checked |
| J10 | `iat` in the future allowed | | H10 | the body's protocol version not compared |
| J11 | `jti` not required | | H11 | `Mcp-Method` not compared |
| J12 | `client_id` not required | | H12 | `Mcp-Name` not compared |
| J13 | `sub` not required | | H13 | bodies of any size read |
| J14 | a key set with two keys of one `kid` allowed | | H14 | a closed connection never cancels |
| J15 | a key set of any size read | | H15 | a cancelled call still reserves and runs |
| K1 | redirects followed | | H16 | `offline_access` allowed in `scopes_supported` |
| K2 | metadata `issuer` not compared | | H17 | a principal's calls in progress not limited |
| K3 | documents of any size read | | H18 | a principal's share of a quota not checked |
| K4 | documents of any depth read | | | |
| K5 | an unknown `kid` fetches every time | | | |
| K6 | keys used however old | | | |

## 5. Fuzzing

Seven libFuzzer targets ([fuzz/README.md](../../fuzz/README.md)), with corpora
kept from earlier runs and seeded from the repository (`scripts/fuzz_seed.sh`). Each
was run for 15 minutes on one x86_64 Linux machine during the preparation of this
package; the target code is that of the reviewed commit. No run found a fault in
Staal.

| Target | Sanitizer | Inputs run | Time | Found |
| --- | --- | ---: | --- | --- |
| `parse` | none | 304,077 | about 14.5 min (stopped by the run's overall limit) | nothing |
| `format` | none | 123,640 | 15 min | nothing |
| `journal` | none | 404,451 | 15 min | nothing |
| `manifest` | none | 4,096,861 | 15 min | nothing |
| `config` | none | 22,712,975 | 15 min | nothing |
| `jwt` | none | 166,537,359 | 15 min | nothing (after the target's own fault was fixed, SELF_ASSESSMENT.md F9) |
| `config` | AddressSanitizer | 6,975,902 | 15 min | nothing |
| `journal` | AddressSanitizer | 223,220 | 15 min | nothing |
| `engines` | AddressSanitizer | 203,933 | 15 min | nothing: the engines agreed on every input |

Fifteen minutes per target is a smoke test, not a campaign. The weekly CI job runs
every target for 20 minutes with AddressSanitizer (`.github/workflows/fuzz.yml`),
from repository seeds only, since CI keeps no corpus.

## 6. Static checks

- `cargo clippy --all-targets --features failpoints -- -D warnings`, in CI, with
  `undocumented_unsafe_blocks` and `missing_safety_doc` on for the library.
- `cargo deny` and `cargo audit` ([SUPPLY_CHAIN.md](SUPPLY_CHAIN.md)).
- Formatting by `rustfmt` is not enforced.

## 7. What this evidence does not show

- **Soundness of the checker.** The fixtures are the attempts made to get past each
  rule; they do not prove there is no other way (SCOPE.md Q1).
- **Memory safety of native code beyond what `engines` reaches**: it fuzzes the
  configs of the 424 runtime cases, not new programs, so code generation is fuzzed
  only for the programs in `tests/run`.
- **Behaviour against the real services**: Claude, Stripe, Termii, Resend and Slack
  are tested against stand-ins written from their documentation.
- **Load and time**: no test runs for hours, with many connections, or with large
  journals.
- **Windows**, and the platforms in [LIMITATIONS.md](LIMITATIONS.md) section 5.
- **Everything about the HTTP transport's interoperability** beyond the two official
  SDK clients.
