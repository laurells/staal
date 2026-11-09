# Threat model

Staal's threats, what mitigates each, where in the code, which tests check it, and
what remains. The method is STRIDE (Spoofing, Tampering, Repudiation, Information
disclosure, Denial of service, Elevation of privilege) applied to each trust
boundary of [ARCHITECTURE.md](ARCHITECTURE.md). Accepted risks are stated here and
in [LIMITATIONS.md](LIMITATIONS.md); a reviewer should treat every "Mitigated"
below as a claim to test, not as settled.

Written by the implementation's author. That is the reason for the independent
review; it is also why each entry names its evidence, so it can be checked rather
than believed.

## 1. Assets

| Id | Asset | Why it matters |
| --- | --- | --- |
| A1 | **Authority**: the operator's grants and their limits (`each`, `max`, `uses`, `only`, currency) | the boundary between what a model may cause and what it may not |
| A2 | **Money and side effects** in external services (refunds, messages, database rows) | the harm Staal exists to bound |
| A3 | **Exactly-once**: no action repeated by a crash or a resent call | a duplicated refund or message is real loss |
| A4 | **Secrets**: API keys, database passwords, the TLS private key, the principal key | their disclosure gives authority outside Staal |
| A5 | **Callers' data and runs** in a shared host (tenants, principals) | one caller must not see or repeat another's operations |
| A6 | **Integrity of the program**: the code that runs is the code that was checked, packages included | the compiler's guarantees are about that code |
| A7 | **Availability** of a host serving tools | a caller must not stop others from being served |
| A8 | **Durable state**: journals, ledger, waiting records | recovery decisions are made from them |
| A9 | **The release** users install | the code users run is the code that was built and tested |

## 2. Actors and what they can do

| Id | Actor | Capability assumed |
| --- | --- | --- |
| X1 | **A model**, possibly turned by injected text | chooses which tool to call and every argument the schema offers; writes any text where a program lets it; sees what the program shows it |
| X2 | **An MCP client or app caller** | sends any request any number of times, concurrently, at any moment, including resending after a lost answer |
| X3 | **A network attacker** (HTTP transport) | sends arbitrary bytes to the listening port: malformed HTTP, slow clients, smuggling attempts, forged or replayed tokens, many connections |
| X4 | **A holder of a valid token** | everything X3 can, as one principal |
| X5 | **A hostile or compromised service** a program talks to | answers anything: oversized, malformed, slow, never, echoing secrets, redirecting |
| X6 | **A malicious package author or registry writer** | publishes package code and registry entries; controls a git repository |
| X7 | **A process crash or power loss** | stops the process at any instruction, including mid-write |
| X8 | **A local user without access to the journal directory** | can connect to loopback ports and read world-readable files |

Out of the model (trusted, [GUARANTEES.md](../GUARANTEES.md) section 2): the
operator, its run configs, manifests and grants; an app's own code computing grants
and app inputs; the library capabilities the operator grants keys to; C code granted
through `cap ffi`; whoever can write the journal directory or edit both
`staal.lock` and `.staal/packages/`; the machine (its disk honours `fsync`); the
authorization server the operator names, as an issuer of tokens.

## 3. Threats

Each row: threat (actor, STRIDE), mitigation, evidence, residual. File references
are to `src/` unless given otherwise; tests are in `tests/` unless given otherwise.

### 3.1 The compiler and the language (B1, B2, B5)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T1 | X1 makes model output act directly: a proposed value reaches an action unvalidated (E) | E001: `Proposed<T>` must pass `validate` before an action or a plain return (`checker.rs`) | 19 E001 fixtures in `tests/fixtures/fail`; GUIDE.md "What the compiler enforces" | a soundness bug in the checker's flow analysis; this is the core review question (SCOPE.md Q1) |
| T2 | A program acts without, or beyond, a capability (E) | E002 (no capability in scope), E003 (widening by passing, assigning, returning or reading); the broker checks the whole chain again at run time (`interp/broker.rs`) | 8 E002 and 21 E003 fixtures; `caps__*` runtime cases on both engines (`engines_test.rs`) | as T1 |
| T3 | X1's text becomes an instruction to a model or SQL (E, T) | E006: untrusted values never reach an instruction position, an `@trusted` slot or SQL text; SQL takes them as bound parameters | 40 E006 fixtures; GUARANTEES.md G4 | a model obeying text inside data it was given (accepted: Staal bounds what obeying can do, not whether the model obeys) |
| T4 | Blinded data leaks to a model through inputs, control flow or a callee (I) | E007, including implicit flows through the decision to call | 99 E007 fixtures | covert channels beyond the call's existence and inputs (timing, cost) are not modelled |
| T5 | X1 drives unbounded model use (D, cost) | E004 (worst-case use against a declared `budget`), E005 (no model call in an unbounded `loop`); budgets enforced at run time; `max_steps`, `max_depth` | 16 E004 and 3 E005 fixtures; `claude_test.rs` charges | a budget the operator sets too high |
| T6 | Evidence claims work a function did not do (R) | E008: `done(evidence: ...)` must cite proof of the function's own work | 16 E008 fixtures | — |
| T7 | Interpreter and native code disagree, so a check passes on one engine (E, T) | one reference (the interpreter); native code calls the interpreter's own functions for every non-Int operation; Int arithmetic checked in machine code | every runtime case run on both engines with results, logs and journals compared (`engines_test.rs`, 22 tests, among them every one of the 424 cases in `tests/run`); `aot_test.rs` runs every case as an executable | an engine difference no case exercises |
| T8 | Lexer or parser crash, hang or memory blow-up on hostile source (D) | hand-written lexer and parser; nesting and size bounded where input is untrusted | fuzz targets `parse` and `format` (ASSURANCE.md) | source files are operator input; a hang there is a bug, not an attack path |

### 3.2 The runtime: broker, capabilities, values (B4, B6)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T9 | An action exceeds a money limit, or is in another currency (E) | the broker checks `each`, `max`, `uses` and currency on every action against the grant and every capability it was narrowed from; a limit that cannot be evaluated refuses (fail closed); ordering two currencies fails (`value`) rather than answering false | `library_test.rs` (money limits before anything is sent; wrong currency refused); GUARANTEES.md G2; attack suite `examples/finops/attacks.py` attacks 2 and 9 | a library that reports a false amount (`@amount`): trusted, RUNTIME.md section 13 |
| T10 | An action reaches a target outside `only` (E) | targets made canonical before comparison (`targets.rs`); `only` patterns never let `*` stand for part of a host; HTTP URLs with traversal refused before sending | `http_test.rs` `only_and_traversal_are_refused_before_anything_is_sent`, `data_placed_in_a_url_stays_one_segment`; `library_test.rs` `only_is_checked_against_the_canonical_target_before_the_library_runs` | a library's `canonical` that maps two targets to one form (trusted, RUNTIME.md section 13) |
| T11 | Concurrent tasks overspend a shared capability (E) | a capability limited by `uses` or `max` anywhere in its chain cannot be passed to `parallel` tasks | `parallel_test.rs` `a_capability_counted_by_uses_or_max_is_never_shared_by_tasks` | — |
| T12 | Header injection or request smuggling through values a program puts in HTTP headers (T) | header names and values checked; CR, LF and reserved names refused; hop-by-hop and framing headers cannot be set by a program | `http_test.rs` `headers_cannot_smuggle_or_override` | — |
| T13 | X5's response is treated as authority or exhausts memory (E, D) | responses are data (`Response`), whatever their status; bodies capped at `net.max_response_bytes`, headers at 128 fields; non-UTF-8 marked incomplete; every network operation under connect and total timeouts | `http_test.rs` `responses_are_data_whatever_their_status`, `a_body_over_the_limit_is_marked_incomplete`, `a_response_holds_up_to_128_header_fields_and_a_value_must_be_text` | — |
| T14 | A program reads or writes outside its file grant via links, `..`, special files or races (E, T) | paths canonical and checked against `only`; every folder opened in turn with `openat` and `O_NOFOLLOW`, no link followed, regular files with one name only; FIFOs and devices refused without blocking; writes atomic (temporary file, `fsync`, rename, directory `fsync`) (`interp/fsx.rs`) | `http_test.rs` `files_are_read_and_written_inside_only`; `fsx.rs` unit tests including `refuses_special_files_without_blocking` | — |
| T15 | SQL injection through model or service text (T, E) | E006 at compile time; parameters bound; SQLite opened refusing links and with `ATTACH` forbidden by limits; PostgreSQL `query` in a READ ONLY transaction | `db_test.rs`, `postgres_test.rs`; GUARANTEES.md G4 (attack 3) | — |
| T16 | Outbound TLS accepts a forged certificate (S, I) | rustls with Mozilla's roots plus operator-added roots only (never instead); host name and validity checked; a failed handshake sends nothing | `https_test.rs` (11 tests: unknown authority, self-signed, another name, expired, not yet valid); `tls_test.rs`; `postgres_test.rs` `tls_is_verified_against_the_operator_s_roots_and_the_host_name` | trust in the bundled roots: three advisories against them were found and fixed in this review cycle (SUPPLY_CHAIN.md) |
| T17 | A listening program (`net.tcp.accept`, `net.tls.accept`) is held by slow or bad clients (D) | each request read within `net.timeout_ms` and `net.max_response_bytes`; bad clients dropped and logged by default; optional `wait_ms` bounds the wait, checked before each client is taken | `serve_test.rs` (16 tests); `tcp.rs` unit test `an_accept_ends_at_its_wait_limit_while_bad_clients_keep_arriving` | without `wait_ms`, an accept waits for ever by design (refused in a tool) |
| T18 | TLS listening with an unusable or exposed key (I, S) | certificate parsed and in its validity period, key matches the certificate, key file a regular file of the user with mode 600 or 400, refused at start; TLS 1.2 and 1.3 only | `tcp.rs` unit tests (`a_certificate_and_its_key_are_checked_before_listening`, three more); `serve_test.rs` TLS tests | outside Unix it is refused (the key file's permissions cannot be checked) |
| T19 | A refusal message shows hostile text that misleads its reader (S, T) | outside text inside runtime messages is escaped (control, line separators, bidirectional characters, backslashes) and cut to 100 characters (`clip.rs`) | `clip.rs` tests; GUARANTEES.md G9 (attack 3) | a program's own `fail(...)` text is the program's |

### 3.3 Durability and recovery (B11)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T20 | A crash makes an action happen twice (T) | intent journaled and `fsync`ed before every operation, result after; a completed operation is replayed, never redone; one in doubt is resent only with the same idempotency key to a service the operator says deduplicates, otherwise the run stops for an operator | `crash_test.rs` (14 tests: every crash point, both engines, concurrent tasks, TCP, listening over TCP and TLS); `http_crash_test.rs`; `library_crash_test.rs`; `claude_test.rs` `a_crash_at_every_point_resumes_to_the_same_answer` | an operator who claims a service deduplicates when it does not (GUARANTEES.md section 2) |
| T21 | Recovery guesses the outcome of an action in doubt (T) | never: in doubt stops the run; `staalc resolve` takes the operator's record; actions safe to repeat (reads, `repeatable` C calls) are made again | `crash_test.rs` `an_in_doubt_action_is_never_guessed`; `ffi_test.rs` `a_call_cut_short_is_in_doubt_unless_repeatable` | — |
| T22 | A resume runs different code or config against an old journal (T) | the journal records hashes of the program (every module and package) and the config; resume requires both to match | `runconfig_test.rs` `a_run_from_the_command_line_resumes_only_with_the_same_inputs`; RUNTIME.md section 11 | — |
| T23 | Non-determinism (time, random numbers, console input) diverges on replay (T) | recorded in the journal and replayed; a tampered record is refused | `crash_test.rs` `what_differs_between_runs_is_replayed_after_any_crash`, `a_tampered_random_number_or_time_is_refused` | — |
| T24 | A torn write corrupts the journal or ledger (T, D) | append-only; only a torn last line is accepted and cut; any other damage refused; a failed ledger write poisons it until restart | `journal.rs` `a_torn_last_line_is_discarded`; fuzz target `journal`; `ledger.rs` tests | a disk that acknowledges `fsync` without writing (trusted) |
| T25 | Whoever can write the journal directory forges history (T, E) | out of scope (trusted); the host refuses a journal directory that is not the user's or is writable by others | `tool.rs` `private_dir`; SECURITY.md "Out of scope" | as stated |

### 3.4 Secrets (A4)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T26 | A secret reaches the journal, the log, a model, an error message or the client (I) | secrets come only from the environment (never inputs or config values); recorded by variable name only; every secret value removed from response bodies, file reads and log events; printable ASCII without quotes or backslashes so it cannot change a JSON or header structure | `http_test.rs` `a_secret_reaches_only_the_header_and_never_the_log`, `a_secret_in_a_json_body_and_the_journal_never_holds_it`, `secrets_come_only_from_the_environment`, `text_from_outside_cannot_ask_for_the_key`; `mcp_crash_test.rs` checks every file for the key | C code given a secret (trusted); a secret transformed (encoded, split) by a service before it is echoed |
| T27 | The notify command or a C library learns the host's secrets (I) | notify runs with a cleared environment, only the variables `notify_env` names (which no grant may use), no extra descriptors | `mcp_test.rs` `a_notify_variable_may_not_hold_a_secret_a_tool_uses` | C code (trusted, marked unchecked) |

### 3.5 The tool host (B2, B7)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T28 | X2 overspends a quota with concurrent calls (E) | each call reserves its worst case in the ledger, under one lock, before it starts; settled to the actual charge | `mcp_test.rs` `quotas_hold_across_calls_and_restarts`; `python/tests/test_tool.py`; attack 5 | quotas per tool across callers (per-principal shares exist only over HTTP) |
| T29 | X2 repeats an operation by resending (T) | `once`: a key over the arguments that identify the operation, written with the reservation, before the run; a repeat gets the earlier run's answer; the same key with other arguments is refused | `mcp_test.rs` `once_makes_a_call_one_operation_however_often_it_is_sent`; `mcp_crash_test.rs` (every crash point) | a `once` key that does not name what makes two calls the same (operator's choice) |
| T30 | Arguments become authority (E) | arguments are inputs only; grants, app inputs and scope come from the manifest or the app; an argument naming an app input is refused | GUARANTEES.md G1 (attacks 1, 3); `mcp_http_test.rs` `app_inputs_come_from_the_token_and_never_from_the_caller` | — |
| T31 | One tenant sees or repeats another's runs (I, T) | a trusted scope per call (from the app, or the principal over HTTP) holds `once` keys and status; a run asked for from another scope is `unknown` | Python and Node.js scope tests; `mcp_http_test.rs` `each_principal_has_its_own_once_keys_and_runs` | — |
| T32 | X2 holds the host (D) | concurrency limits refuse rather than queue; threads bounded; every network wait and SQLite statement timed out; `deadline`; accept only with `wait_ms` | `mcp_test.rs` `concurrency_is_bounded_and_refused_calls_reserve_nothing`; `deadline_test.rs` | C code cannot be interrupted (warned at start) |
| T33 | A clock jump frees quota or expires `once` keys (T) | while open, the ledger reads the clock as at most a minute ahead of time since boot; across a restart a jump is warned about | `ledger.rs` tests | a forward jump across a restart (accepted, LIMITATIONS.md) |
| T34 | Two processes act on one ledger (T) | an exclusive OS lock per journal directory | `host/mod.rs` `Host::open` | — |

### 3.6 The MCP server over stdio (B2)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T35 | Something other than protocol messages reaches the client's stream, or something reads the client's input (T) | descriptors 0 and 1 duplicated to private ones before anything runs; 0 pointed at `/dev/null`, 1 at standard error; one lock around each whole message | `mcp_test.rs` `nothing_but_protocol_messages_reach_standard_output`, `a_c_library_writing_to_standard_output_never_reaches_the_stream` | — |
| T36 | Oversized or deeply nested messages exhaust memory (D) | lines over 4 MiB are read to their end without being kept, and refused; nesting over 64 refused before parsing; batches refused | `mcp_test.rs` `both_protocol_versions_are_served_and_what_mcp_forbids_is_refused`; `proto.rs` tests | — |

### 3.7 The MCP server over HTTP, with OAuth (B3, B8; not in a release build)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T37 | Request smuggling or framing confusion (T, S) | HTTP/1.1 framing by hyper; a request with both `Content-Length` and `Transfer-Encoding` refused | `mcp_http_test.rs` `bodies_are_framed_by_hyper_and_bounded` (the smuggled request never served) | a defect in hyper |
| T38 | DNS rebinding or a browser page calls the server (S) | `Host` must be in `allowed_hosts` (421); `Origin`, when present, in `allowed_origins` (403) | `mcp_http_test.rs` `each_check_refuses_with_its_own_answer_in_order` | — |
| T39 | Unauthenticated access (S, E) | every request needs a valid bearer token; no unauthenticated mode in a release build | `mcp_http_test.rs` `requests_need_a_valid_token_and_tools_their_scopes`; `a_release_build_refuses_http_until_it_has_authentication` | — |
| T40 | A forged, altered, expired or mis-addressed token is accepted (S, E) | strict RFC 9068 profile: `typ` `at+jwt`, `alg` in RS256/PS256/ES256/EdDSA only, no `crit`, key by `kid` only (header keys never used), RSA at least 2048 bits, signature by ring, exact `iss`, `aud` naming the resource, `exp`/`nbf`/`iat` with leeway, `sub`/`client_id`/`jti` required | `mcp/jwt.rs` tests: 38 tokens from PyJWT (`tests/fixtures/jwt/`), RFC 7515 and 8037 vectors; fuzz target `jwt` (no made-up token verifies); deliberate faults (ASSURANCE.md section 4) | the issuer itself (trusted); a token valid until it expires (no revocation, LIMITATIONS.md) |
| T41 | A token for another audience or an ID token is replayed (S) | `aud` must name this resource; `typ` must be `at+jwt` | jwt cases "another audience", "aud array without the resource", "typ JWT", "no typ" | authorization servers that do not set `at+jwt` cannot be used (LIMITATIONS.md) |
| T42 | Key discovery is redirected or poisoned (S, T) | metadata and keys fetched over verified HTTPS only, no redirect followed, 5 s connect and 10 s total, 256 KiB, nesting 16, fields checked; metadata `issuer` must equal the configured issuer exactly; duplicate `kid` refused | `mcp_keys_test.rs` (5 tests); deliberate faults (ASSURANCE.md section 4) | the authorization server's own host (trusted) |
| T43 | Made-up `kid`s make the server fetch keys without bound (D) | at most one fetch a minute for an unknown `kid` | `keys.rs` test; `mcp_http_test.rs` `a_rotated_key_is_fetched_once_and_made_up_keys_fetch_at_most_once_a_minute` | — |
| T44 | Revoked keys stay valid while the authorization server is unreachable (S) | a key set is used for at most 24 hours after the fetch that got it | `keys.rs` `keys_are_fetched_again_only_as_the_rules_say` | up to 24 hours (accepted, LIMITATIONS.md) |
| T45 | A token holder calls a tool beyond its scopes (E) | each tool's `scopes` must all be in the token; 403 `insufficient_scope` names them all | `mcp_http_test.rs` `requests_need_a_valid_token_and_tools_their_scopes` | scopes compared as exact strings; no hierarchy |
| T46 | A principal reads or repeats another's runs, or a subject is recognisable in the ledger (I, T) | the call's scope is HMAC-SHA-256 keyed by the server's principal key over a length-prefixed (iss, sub); the subject is never written; a changed key is refused while principals' runs are pending | `principal.rs` (independent HMAC vectors); `mcp_http_test.rs` `each_principal_has_its_own_once_keys_and_runs`, `a_changed_principal_key_cannot_hide_callers_runs`, `after_a_crash_one_principal_never_gets_or_repeats_anothers_refund` | — |
| T47 | A token holder starves others (D) | per principal: request rate (429), calls in progress (busy), a durable share of a tool's quota; per server: `max_connections`, header and body time limits, 4 MiB bodies | `mcp_http_test.rs` the three `a_principal_*` tests, `connections_over_the_limit_are_closed_at_once`, `bodies_are_framed_by_hyper_and_bounded` | requests without a valid token are bounded only by connections and time limits; principals together can reach a tool's own limits (by design) |
| T48 | Cancellation undoes or misreports an action (T, R) | a closed connection cancels only before the reservation; after it the run finishes and is recorded | `mcp_http_test.rs` `a_connection_closed_before_the_reservation_does_nothing_and_after_it_refunds_once`; deliberate faults (ASSURANCE.md section 4) | — |
| T49 | The token, the TLS key or the subject reaches a log or file (I) | refusals logged by rule without the token (rate-limited); the key stays in rustls; the subject only as its HMAC | `mcp_http_test.rs` token, key and subject checks over every file and log | — |
| T50 | A stop on SIGTERM drops answers already made (R) | graceful shutdown: connections finish their request; calls finish and are answered; at most 10 s to write | `mcp_http_test.rs` `sigterm_lets_the_call_in_progress_finish_and_be_answered` (found the bug this fixed) | a client that never reads its answer loses it after 10 s |

### 3.8 The C interface, Python and Node.js (B7)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T51 | NULL, non-UTF-8 or malformed arguments crash the caller's process (D) | every argument checked; every function's body runs under `catch_unwind`; a panic becomes an `error` result and never unwinds into C | `capi_test.rs` `arguments_that_are_not_strings_are_errors_not_crashes` | a caller that passes a dangling pointer (C's contract, `include/staal.h`) |
| T52 | Memory is freed twice or leaked across the boundary (T, D) | every returned string from `CString::into_raw`, freed only by `staal_free`; the Python and Node.js bindings free each exactly once | UNSAFE.md (every block's justification); Python and Node.js test suites | — |

### 3.9 Native code and C code (B9, B10)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T53 | Generated code corrupts memory (T, E) | native code holds values only as arena handles checked on every use; Int operations checked; the runtime helpers are `unsafe extern "C" fn`s whose contract is stated, each raw pointer turned into a reference once | UNSAFE.md; `engines_test.rs`; `aot_test.rs` | a Cranelift code generation bug |
| T54 | An executable from `staalc build` runs different code (T) | at start it re-checks its embedded program and refuses to run unless its fingerprint matches and its native functions are exactly the compiled ones | `aot_test.rs` `a_damaged_program_is_refused` | package authority is not re-checked at start (it was at build) |
| T55 | C code does anything the process can (E) | accepted: granted only by the operator, every such run marked unchecked in its log, results and docs; the library pinned by SHA-256; calls journaled and serialised per library | `ffi_test.rs` (10 tests, including `only_the_pinned_library_is_loaded`, `a_run_that_loads_c_is_marked_unchecked`) | everything C can do (LIMITATIONS.md) |

### 3.10 Packages and registries (B5)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T56 | Fetched package code differs from what was locked or reviewed (T) | every file's SHA-256 in `staal.lock`, checked at every load; files read from git objects, never a checkout; a moved tag or rewritten history stops the fetch | `packages_test.rs` `a_tampered_package_is_refused_before_it_is_read`, `the_lockfile_itself_is_checked`, `files_are_taken_from_git_objects_not_a_checkout` | whoever can edit both the lock and `.staal/packages/` (trusted) |
| T57 | A package's paths escape its folder or confuse the file system (T) | links, submodules, empty, `.`, `..` and `.git` parts, backslashes, colons, case-only differences, over 10,000 files or 64 MiB refused | `packages_test.rs` `a_repository_with_a_link_or_unsafe_path_is_refused` | — |
| T58 | A package acts beyond what its manifest declares (E) | E108: every capability type in a package names a declared path within its declared patterns; a capability passed into a package is narrowed to it | `packages_test.rs` `a_package_is_held_to_its_declared_authority` | a manifest that declares broad authority (reviewed when fetched: `fetch` prints it) |
| T59 | Argument or command injection into `git` through a location or commit (E) | locations limited to `https://`, `ssh://`, `file://`, absolute paths and plain `user@host:path`, never starting with `-`, no control characters; commits exactly 40 lowercase hex digits; `--` before locations; `git` run with a protocol allow-list (`https`, `ssh`, `file` only; no `ext::`) and no prompts (`GIT_TERMINAL_PROMPT=0`, standard input closed) | `packages.rs` unit test `commits_and_locations_are_exact` | `git` itself; the user's own git configuration (credential helpers) applies |
| T60 | Two versions or two sources of one package confuse the build (T) | one version of each package per project; a package from both git and the registry refused | `packages_test.rs` `one_version_per_package` | — |
| T61 | A registry writer publishes under any name (S) | accepted: the registry is a git repository whose every change must be reviewed; a published version never changes (its entry names a commit and a digest) | RUNTIME.md section 14 "Registries" | a registry without review (operator's choice) |

### 3.11 Distribution and CI (B12)

| Id | Threat | Mitigation | Evidence | Residual |
| --- | --- | --- | --- | --- |
| T62 | A tampered or replaced release archive is installed (T) | `install.sh` checks the archive's SHA-256 before unpacking; with the GitHub CLI it checks the build provenance attestation (release workflow, tag, GitHub-hosted runner) and refuses a failure; installs under the user's home only, never as root | `install_test.rs` `install_sh_installs_a_release_and_refuses_a_bad_download`, `install_sh_checks_the_build_provenance_when_it_can` | without the GitHub CLI only the hash is checked, which detects corruption, not a replaced release; releases up to 0.26.0 have no attestation |
| T63 | A compromised dependency (S, T, E) | every Rust dependency pinned at least two weeks old; `cargo-deny` and `cargo-audit` on every push and weekly; crates.io only | SUPPLY_CHAIN.md; `.github/workflows/supply-chain.yml` | a malicious release older than two weeks and not yet reported |
| T64 | A moved action tag runs other code in CI with publishing rights (T, E) | every action pinned to a full commit SHA; workflows read-only by default; `id-token: write` only on the publishing jobs and the documentation site's deployment (`pages.yml`); no `pull_request_target` | `.github/workflows/` | — |
| T65 | A stolen token publishes a package (S) | PyPI and npm trusted publishing (OIDC); no publishing token stored; npm packages published with provenance | `.github/workflows/publish.yml`; RELEASING.md | the first npm version was published by hand |
