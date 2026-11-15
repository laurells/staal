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
| Integration tests | 297, in 45 files | `tests/*_test.rs` |
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
| HTTP and files | 17 | [4.1](#41-http-and-files) |
| Library capabilities | 16 | [4.2](#42-library-capabilities) |
| Packages | 17 | [4.3](#43-packages-versions-and-registries) |
| Versions and registries | 12 | [4.3](#43-packages-versions-and-registries) |
| C libraries | 18 | [4.4](#44-c-libraries) |
| Native engine | 27 | [4.5](#45-native-engine) |
| Executables | 5 | [4.6](#46-executables) |
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

### 4.1. HTTP and files

How this is tested: 16 runtime tests against a local HTTP server check what the
server received as well as what the program saw (secrets only in headers and
bodies, never logs or journals; traversal and refused targets never sent; a
`{secret}` smuggled into message text refused). `tests/http_crash_test.rs` kills a
run at all 14 points of a program with a GET, a deduplicated POST, a plain POST and
a file write, and checks each POST took effect exactly once. File access is tested
against links in folders and files, loops, hard links, FIFOs, and a folder swapped
for a link 11,000 times during 20,000 reads, with no leak. Seventeen mutations of the
guarantees are each caught: skipping URL normalisation, not removing secrets from
responses, recording a secret's value in the journal, allowing a second
`{secret}`, retrying every POST, not retrying a GET, dropping the
Idempotency-Key, reporting "may have happened" as "not sent", following
redirects, following a link in a folder, following a link as the file,
reading a file with other hard links, treating every TLS error as "not sent",
treating certificate errors as "may have been sent", replacing Mozilla's roots
with the extra ones, ignoring the extra ones, and accepting a private key in a
root file.

### 4.2. Library capabilities

How this is tested: 13 runtime tests run the two libraries against a local server
and check what it received: the canonical number and the key in the body (never in
the log), refusal by `only` before any request, `uses`, `each` and `max` on the
library's amount, the operator's `only` on the library's requests, a library's
declared limits narrowing the grant, a receipt refused when no request was made,
a failed action charged after a POST but not after a GET, data kept to one URL
segment, and config errors in `needs`. `tests/library_crash_test.rs` kills a run
that refunds through Stripe and texts through Termii at all 11 points, resolves the
SMS from what the server received when it is in doubt, and checks the refund took
effect once (same Idempotency-Key on every attempt) and the SMS reached the server
once. Sixteen mutations are each caught: skipping `canonical`, skipping the
broker's check, not counting a use, not narrowing a need, giving a receipt without
a request, not charging on success, not charging after a failed POST, charging
after a failed GET, accepting `needs` on a plain capability, starting a run inside
a capability, turning off the compiler's target, amount and request checks,
hashing file names instead of module paths, and letting `url_segment` pass a `/`
or `..`.

### 4.3. Packages, versions and registries

How this is tested: 12 tests build git repositories and fetch from them. They
check the four reference packages fetch, check, and run against a local server
standing in for each service (including Slack's "ok": false refusal and an email
address `canonical` refuses); tampering by a changed, added
or removed file or a link is refused; an edited lock is caught by the next fetch;
branches, unknown commits, misnamed packages, plain http and links in a repository
are refused; imports resolve inside packages and across dependencies; two
versions of one package are refused; each authority rule; a program runs through a
fetched package against a local server; and a journal refuses to resume on a new
commit. Seventeen mutations are each caught: not comparing hashes, ignoring an
added or missing file, following a link, accepting a link from git, trusting the
lock without git, accepting a branch name, not prefixing a package's own imports,
letting the program import a package it does not depend on, allowing a dependency
and a folder of one name, each of the four authority rules, allowing two versions,
allowing plain http, and accepting a misnamed package. Nine more tests cover
versions and registries: the newest fitting release chosen and kept until
`update`, a moved tag or registry entry, conflicting ranges, an exact commit
against a range, pre-releases, yanking, a registry entry whose files do not match,
and publishing; twelve mutations of the resolver and registry are each caught.

### 4.4. C libraries

How this is tested: ten tests call a C library built from `tests/ffi/staaltest.c` on
both engines: every integer width and sign, bool, text both ways and six arguments;
values that do not fit refused before the call; a wrong hash, path, key or limit, a
missing function and a library anyone may change refused; the unchecked marks; replay
without calling again; a call cut short by a crash, and a C function that aborts, left
in doubt; `repeatable`; `uses`; and concurrent calls made one at a time. Fourteen
invalid programs and three package cases cover the checker. Eighteen mutations are
each caught.

### 4.5. Native engine

How this is tested: all 419 runtime cases run on both engines with a journal, and
their results, logs and journals are identical (3,935 functions compile across them).
2,500 generated programs using Int, Bool, Text, money, records and lists, compiled
entirely to native code and run under tight step and depth limits with values at
their edges, end identically on both engines; across them, runs finish, fail with
`value` and with `fail`, and reach step and depth limits. A table of programs hits
every failure native code reports at its edge. A program mixing both engines is run
at every step limit from 1 to past its end. A 10,000-iteration loop never holds more
than 12 values at once. The crash tests run with every pairing of engines for the
crash and the resume. Twenty-seven mutations of the native compiler are each caught,
among them: a missing step per statement or per `loop`, `repeat` or `for` iteration;
limits off by one; unchecked overflow, `MIN / -1` or negation; no short-circuit; a
value not dropped on `set`, `break` or the end of a block; a variable read without
copying; record fields stored in the order written; a helper's failure ignored;
swapped operands; and a field read that moves the record out of its variable. A
Python script runs every clinic case through the shared library on both engines.

### 4.6. Executables

How this is tested: every runtime case, run by its built executable, gives the same
exit code, output, log and journal as `staalc run`; a program with modules runs as
an executable; a run crashed at every journal point by the executable resumes under
`staalc`, and the other way round, with the same result and world; an executable
whose embedded program was changed by one character refuses to run; build errors are
reported. Five mutations are each caught: the program check skipped, the native code
table out of order, constants or record layouts lost, and running through the
in-memory compiler instead of the linked code.

### 4.7. Modules

How the rules cross files: the compiler resolves every module into one program
in which each name is qualified by its module (`send` in `sms/termii.staal` is
`sms.termii.send`), then checks it exactly as it checks a single file. Blinding
promises, budgets, trust, evidence and partial operations therefore hold
across modules with no separate cross-module analysis. This is tested by
splitting every test program in two (`tests/split_test.rs`): types and called
functions move to a module (capability declarations too), and every one of 352
splittable fixtures and 355 runtime cases must give identical results. Mutations that hide one module's
model calls, failures or costs from another are each caught.

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

What AddressSanitizer covered: `cargo fuzz --sanitizer address` passes the
sanitizer to rustc only (cargo-fuzz 0.13.1), so it instruments the Rust code of
`staalc` and its Rust dependencies. It does not instrument the Rust standard library
(precompiled; its allocations still go through the sanitizer's allocator), C code
built by dependencies' build scripts (SQLite, ring's C and assembly), or the machine
code Cranelift generates at run time. So `engines` checked the native engine's Rust
helpers and the values they handle. The generated code is not instrumented, and these
results say nothing about its memory accesses. By the code generator's design it
reads and writes only its context header and the argument arrays on its own stack
(UNSAFE.md section 2); that is a property of the generator, examined by review and
exercised by the engines agreeing on every case, not something the sanitizer
establishes.

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
