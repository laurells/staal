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
| MCP HTTP transport, tokens, keys, principals, limits | see below | `scripts/mutants_http.py` |

These were made by hand for each change; apart from the last row there is no script
to rerun them, so they are a record, not a suite. For the HTTP transport, which this
review is the gate for, the faults are in a script anyone can rerun
(`python3 scripts/mutants_http.py`). Its result at the reviewed commit:

MUTANTS_RESULT

## 5. Fuzzing

Six libFuzzer targets ([fuzz/README.md](../../fuzz/README.md)), built with `cargo
fuzz -s none` (no sanitizer). Each was run for 15 minutes at the reviewed commit,
from the corpus kept from earlier runs:

FUZZ_RESULT

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
