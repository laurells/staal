# Changelog

All notable changes to `staalc` are recorded here. Versions follow
[Semantic Versioning](https://semver.org); before 1.0, a minor version may change
the language or its file formats.

## Unreleased

This release refuses requests to private addresses unless a grant lists them, and
changes how C functions run by default. A `net.http.*`, `net.tcp.exchange` or
`net.tls.exchange` grant that reaches `localhost` or an internal service needs
`"private_addresses"` listing its address (section "Changed").

It also changes how C functions run by default. A grant of `cap ffi.*` with no
`isolation` now runs its library contained, which needs Linux on x86_64 or aarch64:
on macOS such a grant is refused until it says `"isolation": "separate"` or `"none"`,
and C code that opens files, sockets or programs fails when contained. Grant those
`"isolation": "none"` to keep the old behaviour.

### Added

- C functions run contained by default (RUNTIME.md section 19, "Isolation"): each
  library in a worker process of its own, started fresh, with an empty environment
  and no inherited descriptors, under two seccomp filters. While the library loads,
  its initialisers may read files but not write one, open a socket, start a program
  or signal another process; after it has loaded, it may only compute. Anything else
  stops the worker, and the call fails saying so. A run whose C code is all contained
  is no longer marked unchecked, and a contained call cut short by a crash of the run
  is made again on resume, since nothing it did can have left its worker.
- `isolation` on an `ffi.*` grant: `contained` (the default), `separate` (a worker
  with no limits: the run is unchecked) or `none` (the run's own process, as before).
- `STAALC_FFI_WORKER`, and `staalc::interp::ffi_worker::use_executable`, name the
  `staalc` to run as the worker where none is found: the Node.js package installs none.
- The address a `net.http.*`, `net.tcp.exchange` or `net.tls.exchange` request
  connects to is checked (RUNTIME.md section 12, "Addresses"): a name is looked up
  once, each address it gives is checked, and only those that pass are connected to,
  so DNS rebinding changes nothing. Public addresses are reached; private ones
  (loopback, private networks, link-local and the rest) only when the grant lists
  them, or a network holding them, in `private_addresses`; cloud metadata services
  only with `"cloud_metadata": true`. agent.json declares both on a grant, and
  `staalc agent describe` reports them as each network grant's `addresses`.

### Changed

- A request from `net.http.*`, `net.tcp.exchange` or `net.tls.exchange` to a private
  address, including loopback, is refused unless its grant lists that address in
  `private_addresses`. Grants that reach a local or internal service must now say so.
- In agent.described.json, a network grant's `ip_isolation: false` is replaced by
  `addresses`.
- A C function that crashes in a worker fails its call instead of ending the run's
  process; every later call to its library in the run fails too.
- `staalc doc` heads C declarations "C libraries" and says how they are run.

## 0.28.0 — 2026-11-10

This release changes the journal format, to version 3. A run left unfinished by
0.27.0 or earlier does not resume on it: before upgrading, let every unfinished run
end (`staalc mcp status` lists them) or resolve it with the `staalc` that started it.

### Changed

- A journal binds the whole program by a new source program fingerprint,
  `staal-program-v3` (RUNTIME.md section 11): every module's path and text, and for a
  project its `staal.json` and `staal.lock` as text and every package's name, git
  source, commit and the path and SHA-256 of each of its files. Before, the
  fingerprint held the modules' text alone, and a package was bound only through the
  modules it supplied. The construction is a netstring grammar, specified in
  RUNTIME.md and `src/identity.rs`, with test vectors produced by a second
  implementation written separately in Python (`scripts/fingerprint.py`).
- A journal binds its run config by the SHA-256 of its RFC 8785 (JSON
  Canonicalization Scheme) form, so the hash is defined by a standard, not by how
  `serde_json` writes JSON. A number that an IEEE double cannot hold exactly (an
  integer beyond 2^53 that is not a double) is refused in a run config rather than
  rounded, so two different configs never hash alike. Numbers are written as
  ECMAScript writes them, checked against Node.js 22 on 6,119 committed vectors.
- The start record names the `staalc` version and the engine (`interpreter` or
  `native`) that wrote it, and records the proxies its requests go through. Each
  resume of an unfinished run appends a `resume` record with the run id, the same
  fields and the time, so a journal shows every `staalc` and engine that worked on its
  run.
- `"proxy": "environment"` (under `net` and under `model`) is read once when the run
  starts, checked, recorded in the start record as the variable it came from, its
  scheme, host and port and the `NO_PROXY` entries, and used exactly as recorded. A
  resume under other proxy variables is refused. The variables are those ureq reads,
  in its order (`ALL_PROXY`, `all_proxy`, `HTTPS_PROXY`, `https_proxy`, `HTTP_PROXY`,
  `http_proxy`, then `NO_PROXY` or `no_proxy`), but stricter: a value that is not a
  proxy URL is refused instead of skipped, and so are a proxy URL with a user name or
  password (it would be written to the journal) and a SOCKS proxy (this build cannot
  use one). `NO_PROXY` entries are trimmed and lowercased.
- A journal of any version but 3 is refused, saying to finish or resolve its run with
  the `staalc` that started it; `staalc resolve` checks the version too.
- A `net.http` URL is parsed once, into one target the `only` check, the journal and
  the request all use: the request is built from the parsed parts, never from the
  program's text, and a URL the HTTP layer would read back as anything else is
  refused. The model provider's `base_url` and the MCP server's `auth.issuer` and
  `jwks_uri` are parsed the same way, so they too now refuse a user name, a password
  or an encoded `/` or `\` in the path, and `base_url` accepts plain HTTP to any
  loopback address (`127.0.0.0/8`, `::1`, `localhost`), as `net.http` does.

### Added

- `staalc schema` reports each grant's `limits` (the limits its type writes) and its
  `consumers`: the program's own code, each package function parameter it can be
  passed to, and each library capability's need, with the boundary each is held to
  and where that boundary comes from, plus `consumer_analysis`, which says whether the
  analysis is complete and what it could not follow (RUNTIME.md section 1).

- `staalc agent check` and `staalc agent describe`: an agent's `agent.json` held to
  its normative form (docs/schemas/agent.schema.json) and to its project, and its
  computed description, `agent.described.json` (docs/schemas/agent.described.schema.json),
  written in RFC 8785 form and compared byte for byte (AGENT_MANIFEST.md sections 4 to 6).

- An `Int` in `staalc schema` (and so in an MCP tool's `inputSchema`) no longer has
  `minimum` and `maximum` of -2^63 and 2^63-1: no IEEE double holds the latter, so many
  clients read it as 2^63, and RFC 8785 cannot write it. The bounds are in its
  `description`; the runtime still refuses an integer outside them.

### Fixed

- `net.http` through a proxy no longer looks the target's host up on this machine
  before sending: the proxy resolves it. Before, a request to a host only the proxy
  could resolve failed as not sent. A proxy that refuses the tunnel is reported as not
  sent.

## 0.27.0 — 2026-11-09

The version reviewed in Staal's first independent security review
(docs/security-review/, tag `security-review-1`), and the first release with a build
provenance attestation for every archive, wheel and npm package. It changes what
programs see: a `net.tcp.accept` client that misbehaves is now dropped instead of
failing the accept, and `Response` gains `headers` (see Changed below). The MCP
server over HTTP is in this release's source but refused by its builds until the
review is done.

### Added

- `Response` holds the response's header fields: `headers`, a list of `HeaderField {
  name, value }`, each name lowercase (field names are case-insensitive), sorted by
  name, a repeated name's values in the order received, secrets removed. They are
  recorded in the journal and replayed exactly; a response recorded before this
  version replays with none (its program could not read them). At most 128 fields,
  the most the HTTP client reads: a response with more is no answer, as before. A
  header value that is not UTF-8 empties `headers` and makes `complete` false, as a
  body that is not text empties `body`; the two are now independent, so headers are
  kept when only the body could not be held. On both engines.
- `email.sendgrid` returns SendGrid's `X-Message-Id` as its receipt (before:
  `accepted for <address>`, still used for a 202 without the header). A 202 is
  acceptance whatever else the answer holds, so it no longer fails when the rest of
  the answer could not be held. The `email` package is now 0.2.0: its receipts
  changed, and it needs a staalc whose `Response` has `headers`, so a project on
  `^0.1` keeps the version it has.
- A `net.tcp.accept` grant can set `"wait_ms"`: once that many milliseconds have
  passed since an `accept` began, it takes no client, even one already waiting, and
  fails (`action`), so it lasts at most `wait_ms` plus `net.timeout_ms`. The wait
  uses `poll(2)`, and a failed accept is recorded and replayed like any other. A run
  killed while waiting takes the next client on resume. On both engines.
- A tool (`staalc mcp`, and tools defined through the C API, Python and Node.js) may
  now hold a `net.tcp.accept` grant if it sets `"wait_ms"`, since every wait in a
  tool must have a limit. Without it the grant is still refused.
- `net.tls.accept` and `net.tls.reply`: listening over TLS (1.2 or 1.3). The grant
  names `certificate_chain` and `private_key` files, read when the run starts; the
  run does not start if a certificate does not parse or is not valid at that moment,
  or the key is not the certificate's, cannot be read, or can be read or written by
  other users (Linux and macOS). A failed handshake makes a bad client. A request
  read to its end must end with the client ending TLS, and a reply ends TLS. A
  connection is answered only by the reply capability of the accept that took it.
  The crash test for listening runs over TLS too. On both engines.

- The first stage of the HTTP transport for `staalc mcp` (docs/MCP.md section 12):
  Streamable HTTP at 2026-07-28 on hyper, with TLS, every request check of section
  12.3, time and size limits, SSE for calls that ask for progress, and a clean stop on
  SIGINT or SIGTERM. Then OAuth (stage 2): access tokens verified under RFC 9068 with
  ring, the authorization server's keys found and refreshed over hardened HTTPS
  fetches, the protected resource metadata, 401 and 403 challenges, tool scopes, each
  caller's runs and `once` keys held apart by an HMAC of its identity, app inputs from
  token claims, and limits per caller (request rate, runs in progress, a durable
  share of a quota). Then the crash, duplicate and cancellation tests over HTTP
  (stage 3), which found that stopping on SIGTERM could drop an answer already made;
  the server now shuts down gracefully. The official Python and TypeScript SDK
  clients sign in (client_credentials, discovered through the protected resource
  metadata) and call tools over it, in CI on every platform. A release build still
  refuses `server.http` until the release
  gate (SDK conformance and an independent security review); only a test build
  serves it.
- Releases carry a build provenance attestation for each archive, wheel and npm
  package (`actions/attest`, signed through Sigstore for the release workflow, its
  run and its tag). `install.sh` checks it with `gh attestation verify` when the
  GitHub CLI is installed and signed in, and refuses an archive that fails; without
  `gh` it installs on the SHA-256 as before and says the provenance was not checked.
  `STAAL_REQUIRE_PROVENANCE=1` refuses to install without the check. Releases up to
  0.26.0 have no attestation.
- The fuzz targets run weekly in CI with AddressSanitizer (`.github/workflows/fuzz.yml`),
  seeded from the repository by `scripts/fuzz_seed.sh`. Two targets are new: `jwt`
  (access tokens and key sets: only the test issuer's own valid tokens verify) and
  `engines` (a runtime case with a changed config gives the same result and log on
  both engines; the target that runs native code).
- `docs/security-review/`: the package for an independent security review.

### Changed

- A client of `net.tcp.accept` whose request is too slow, too long or not UTF-8 is
  now dropped by default, logged as a `client_dropped` event (the capability, the
  address and the reason, never what it sent), and the `accept` takes the next
  client. Before, it failed the `accept` (`action`). A grant with
  `"on_bad_client": "fail"` keeps the old behaviour. A program that caught that
  failure with `try` no longer sees it.

- A program that returns a `Response` gives `headers` in its JSON too, and
  `HeaderField` is a built-in type name: a module that declared its own `HeaderField`
  must rename it.

### Fixed

- `staalc --help` (and `-h`, `help`) prints the usage on standard output and succeeds.
  Before, it answered "unknown command --help" with exit code 2.
- The site (`site/index.html`) states only what the language and runtime do, checked
  against the code: the journal resends an action in doubt only with the same
  idempotency key and only when its grant says the service deduplicates (it said a
  payment is never repeated); the compiler bounds model calls and tokens, while money
  limits are the runtime's (it said the compiler refuses programs that could
  overspend); CI runs on macOS too (it said Linux only). It is now laid out as
  language home pages are: install, features, examples, integrations, the safety
  rules, tooling, platforms and limits, and an index of the documentation.
- The README, the documentation's introductions and the package READMEs state only
  what the code does. The Python and Node.js READMEs said the runtime never repeats an
  action after a crash; it never resends one in doubt blind. The tutorial named Linux
  x86_64 only; the guide listed the Python package and C calls as not built; RUNTIME.md
  called the native engine a future backend; MCP.md promised remote-transport scoping
  and computed narrowing. The README is ordered as language repositories are (what it
  is, installing, quick start, features, command line, documentation, then the
  benchmark, rerun on 0.26.0, and platforms), and `docs/README.md` indexes the
  documentation by Learn, Reference, Tools and Project.
- SECURITY.md said a tool may not listen for connections; with `"wait_ms"` it may. It
  now also covers the HTTP transport and rates severity with CVSS v4.0.
- The release archive and the wheel are built with `cargo build --locked`, so a
  `Cargo.lock` out of step with `Cargo.toml` fails the build instead of being updated.
- SECURITY.md said 0.24.x was the supported version. It now says 0.26.x, and the release
  check requires the supported minor version to be the one being released.

## 0.26.0 — 2026-11-04

This release changes the language: ordering money in two different currencies now
fails instead of answering false. A program that relied on the false answer stops
with a message instead (see Changed below).

### Added

- A tool's run that stops only because actions safe to perform again got no answer (an
  HTTP request whose grant has `"idempotency_key": true`, an action always safe to
  repeat) is resumed within the same call, after 0.25 s, 1 s and 2 s, sending the
  request again with the same key. Before, it waited for the server's next start, and
  its answer said a person had to check, which was not true. A stop that may need a
  person's answer is never retried, nor is a model call, a print or a clock read in
  doubt; a run of parallel tasks is retried only when every operation in doubt is safe
  to repeat. One still unanswered after the third resume is left for the next start
  and the operator, its answer now saying so (docs/MCP.md section 7). The runtime's
  `in_doubt` log event now says `"op"` beside `"retry_on_resume"` (RUNTIME.md
  section 10).
- `examples/finops/`: a financial operations agent (customer records, refunds, booking,
  text messages, daily limits) and `attacks.py`, which attacks it on purpose and judges
  each attack by the stand-in bank's and SMS service's own ledgers, run by the tests on
  both engines. `docs/GUARANTEES.md` states each guarantee, what it depends on, how it
  is tested, and where it stops, with an honest comparison with ordinary application
  code.
- App inputs: a tool defined with `"app_inputs"` takes those inputs of its entry
  function from the app with each call, never from a caller (`app_inputs=` in Python,
  `appInputs` in Node.js, functions of the call's context like `grants`; the new
  `staal_tool_call_with_inputs` in C, with `staal_tool_call` unchanged). They are left
  out of the input schema, a call whose arguments name one is refused, the runtime
  checks their types, and `once.by` may name them. So a tool can read or act on the
  record of the customer the app chose, and a model cannot choose another; before,
  every input came from the caller, and a database grant can only be limited to a
  file, not to rows. Refused in an MCP manifest, whose tools have no app.

- `@staal/node/vercel-ai`: `aiTool` and `aiTools` give a staal Tool to the Vercel AI
  SDK (`ai` 7), an ES module, as the SDK is. The tool's context is its entry in
  `toolsContext`, passed to the Tool's grants and scope functions and never to the
  model; Staal checks the arguments, and a refusal or failure reaches the model as an
  `error-text` output with Staal's message. Tested through `generateText`'s own tool
  loop with the SDK's mock model, with `examples/frameworks/vercel_ai_refund.mjs` run
  as written.
- Claude Agent SDK adapters: `staal.claude_agent_sdk.mcp_server` (extra
  `staal[claude-agent-sdk]`) and `@staal/node/claude-agent-sdk`'s `mcpServer`, an ES
  module, serve staal Tools as an in-process MCP server, with the session's context
  bound when the server is made, since the SDK gives a tool only its arguments; and
  `allowed_tools` / `allowedTools` give their names. The Node server is the MCP SDK's
  own low-level server, so the schema reaches the model unconverted. Tested through
  the SDK's agent loop in the Claude Code CLI it ships, against a local server
  answering as the Messages API does, with the examples run as written.
- `staal.google_adk.adk_tool` (extra `staal[google-adk]`): a staal Tool as a Google
  ADK tool, its schema given as JSON Schema, its context the session's state under
  `"staal_context"`, and its answer `{"result": ...}` or `{"error": ...}` as ADK's own
  tools give them. Tested through the Runner's agent loop with a scripted model, on
  google-adk 2.12.0.
- `docs/frameworks/claude-agent-sdk.md`: a tutorial, a support agent on the Claude
  Agent SDK that can never refund more than the app allows for the customer, in Python
  and TypeScript. Its files are saved and run as written by the tests
  (`python/tests/test_tutorials.py` and the Node framework tests), against a local
  stand-in for the Messages API, and the output the page shows must be what they print.
- The same tutorial for the other frameworks, to the same standard and tested the same
  way: `docs/frameworks/openai-agents.md` and `docs/frameworks/langgraph.md` (Python
  and TypeScript, against stand-ins for the Responses and Messages APIs),
  `docs/frameworks/vercel-ai.md` (TypeScript) and `docs/frameworks/google-adk.md`
  (Python, against a stand-in for the Gemini API). The Node tests now also run every
  JavaScript program whose output a page shows, not only the agent, and a page in the
  list that is missing fails the test.
- A weekly workflow (`frameworks-latest`) runs every adapter test against each
  framework's latest release, so a framework that changes under an adapter is found
  before a user finds it. Its first run, by hand, found a test too strict about the
  Claude Agent SDK's tool list (a newer CLI adds a tool-search placeholder); the test
  now checks the refund is offered and no built-in tool is.
- CI runs Node 22, which the Vercel AI SDK's tests need.
- CI installs the Python test frameworks with `--uploaded-prior-to=P14D`, and the Node
  framework tests' lock file was made with `npm install --before`, so dependencies the
  pins do not name are at least two weeks old too; the one exception is body-parser
  2.4.0, for GHSA-27r3-jxmh-p828.
- The Node framework tests install a copy of the package's own files, as `npm install`
  would, so ES module adapters resolve their frameworks as in an app, and
  `npm run types` checks the ES module adapters' types too.

### Changed

- Ordering money in two different currencies (`<`, `<=`, `>`, `>=`) now fails, as a
  failure of kind `value` that `try` can catch, instead of answering false. A false
  answer let a check like `amount > left` pass unnoticed for an amount in another
  currency: building the finance example found exactly that, where only the broker's
  currency check stopped the refund. `==` and `!=` are unchanged (never equal), as are
  `min`, `max` and `sort`, which already failed. A program that compared amounts the
  caller or a model gives with its own, and relied on a false answer to fall through,
  now stops with a message naming both amounts; check `currency(x)` first to choose
  what happens instead. On both engines.

  At compile time: a comparison that orders money can now fail, so it counts as such
  for E007. A program that orders a blinded amount and then makes a model call is
  rejected, since whether the call happens would reveal the amount's currency; it
  compiled before. Ordering Ints is unaffected: the checker finds which comparisons
  order money from their types (a second checking pass, only for programs that order
  money at all).

### Fixed

- The runtime's own messages show text from outside (a target, an amount, an argument's
  or a field's name, a URL or a path) with control characters, line separators,
  bidirectional formatting characters and backslashes escaped, and cut to its first 100
  characters with its length. Before, a refusal echoed it as it was: a 1 MB argument came
  back as a 1 MB message to the model, and escape sequences reached logs and terminals
  as they were. Found by attacking a tool on purpose; covered by tests in Rust and
  Python. Only what messages show changed: values reach programs, journals and services
  unchanged.
- `staal.Tool` called with arguments that are not a dict raises a TypeError saying so,
  and Node's `Tool.call` likewise, instead of an error from deep inside.
- docs/RUNTIME.md said a receipt is `Receipt { id, capability, method }`, which reads as
  fields a program can use; it holds them, but code cannot read them, and `json(r)` or
  returning it gives `{"receipt", "capability", "method"}`.

## 0.25.0 — 2026-11-02

### Added

- `staalc schema <file> [--entry <fn>]` prints what a caller outside a program sees of
  a function, for describing it as a tool: a JSON Schema (2020-12) of its inputs, one
  of its result, and the grants and secrets only an operator gives. Records and enums
  are `$defs` referred to by `$ref`, so recursive types are exact, and money is
  described with its currency's decimal places. Checked against the runtime's own
  input rules on 39 values: they agree on all but a money amount too large to hold
  exactly, which the runtime refuses and the schema cannot express.
- `limits.deadline_ms`: how long a run (or each resume of it) may keep starting
  actions and model calls. Checked before each one starts, never during one, so it
  never cuts an action in half; fatal (`deadline`), so `try` cannot catch it; and a
  resumed run decides as its journal says it did. On both engines, concurrent tasks
  included.
- A run's result reports `usage`: for each capability parameter of the entry function,
  the actions attempted and the money charged through it, per currency, whatever its
  limits, counting a resumed run's replay. From the C interface, Python and Node.js.
- `staalc schema` reports `reads_console`: whether the function can reach
  `read_line()`, through its calls and the library capabilities it is given.
- `staalc mcp <manifest.json>`: Staal programs served as tools to Model Context
  Protocol clients over stdio, under 2025-11-25 (after `initialize`) and 2026-07-28
  (per request), as docs/MCP.md describes. A tool call is an ordinary run: the
  manifest gives the grants, secrets, model and timeouts, and the arguments are only
  ever inputs. Across calls the server adds quotas (calls and money per window, each
  run reserving its worst case until it settles), concurrency limits, deadlines and
  `once`, which makes repeated calls of one operation one run. A ledger, fsync'd
  before each step, has a recovery rule for every crash point; a run left in doubt
  waits for an operator, who is told by a waiting record and an optional notify
  command run with a cleared environment. `staal_run_status` reports any run;
  `staalc mcp status` lists unfinished runs and quota use. Tested end to end, killed at
  every journal and ledger write, and against the official Python (`mcp` 2.3.0) and
  TypeScript (`@modelcontextprotocol/client` 2.3.1) SDK clients.
- `docs/MCP.md`: the design of `staalc mcp`, kept as its reference.
- Package tests as data: a package's `tests/*.json` describe a fake service's routes
  and answers, a program using the package, its grants, and the result and requests
  expected. `tests/package_cases_test.rs` runs them against a local fake
  (`tests/common/fake.rs`), with the package fetched through git so its manifest's
  authority holds, checks the key appears in no log or journal, and, with
  `--features failpoints`, kills every test at every journal write and between every
  request and its answer, then checks it ends with the same effects: none lost, none
  made twice. `STAAL_PACKAGE=<folder>` runs one package.
- `packages/template`: a package to copy, with one action and its tests.
- `docs/PACKAGES.md`, "Write your first Staal package": Postmark's email API from an
  empty folder to passing tests, run by the test suite as written.
- `packages/code`: GitHub, with `code.github.comment`, `comment_pull` (checked to be a
  pull request first), `label` and `create_issue`.
- `packages/messaging`: Twilio, with `messaging.twilio.sms` and `whatsapp`. Both
  packages are tested against local fakes of the services' documented answers, not
  yet against live accounts.
- Tools in an app: `staal.Tool` and `staal.Host` in Python, `Tool.open` and
  `Host.open` in Node.js, over new C functions (`staal_host_open`, `staal_tool_open`,
  `staal_tool_call`, `staal_tool_close`, `staal_host_close`, `staal_run_status`). A
  program becomes a tool an agent calls many times, with quotas across calls, `once`,
  and crash recovery, from the same code as `staalc mcp` (now `src/host/`). Grants are
  fixed, or computed by the app for each call and checked as a manifest's are; they are
  never an argument. An app serving several tenants or users gives each call a trusted
  scope (`scope=`, from the app's own records): `once` keys are held within it, and a
  run's status is given only in it. One process uses a journal directory at a time.
- Framework adapters: `staal.openai_agents.function_tool` and
  `staal.langgraph.structured_tool` (extras `staal[openai-agents]`,
  `staal[langgraph]`), and `@staal/node/openai-agents` and `@staal/node/langgraph`
  (optional peer dependencies). Tested through each framework's own agent loop, on
  pinned versions, with the examples in `examples/frameworks/` run as written.
- Packages: `email.postmark.send` (the guide's package, finished),
  `email.sendgrid.send` (Twilio SendGrid v3) and `chat.discord.post` (Discord API v10,
  as a bot, with mentions never parsed), each tested against a fake of its documented
  answers. Microsoft Teams is not in this release: Microsoft Graph's tokens expire and
  must be fetched, which a package's fixed `Secret` cannot do yet.
- Package tests run on both engines, the crash tests included.
- `SECURITY.md` says what tools trust and what they do not.
- `CONTRIBUTING.md` says what to expect from review (a first response within two
  working days) and how to take a first issue; `packages/README.md` lists what every
  package must meet.

### Changed

- **Before upgrading:** run `staalc mcp status` and let every unfinished run end
  (resolving any that wait for an operator). A run file or journal left by 0.24 is not
  resumed by this version: 0.24's arguments file is not a run file, so the directory
  refuses to open and names each such run (docs/MCP.md section 7, "Upgrades").
- `staalc mcp` keeps a run file (`<run id>.run.json`: the program, engine and run
  config) in place of the arguments file, so an unfinished run resumes without its
  tool's definition: changing or removing a tool in the manifest no longer stops the
  server from starting. Editing a program or upgrading `staalc` with runs unfinished
  still does. Ledger `reserve` events record the quota window and how long a `once`
  key is kept, so compaction no longer depends on the manifest.
- The ledger reads the clock as at most a minute ahead of the time since boot while it
  is open, so a clock set forward frees no quota and expires no `once` key early until
  the host restarts; a `clock` event keeps the latest time through compaction. Removing
  a file flushes its folder, so a removal is never undone by a power loss after the
  ledger forgot the run.
- A SQLite statement (`db.sqlite.query`, `execute`) is stopped once it runs longer
  than `net.timeout_ms` (30 s by default), as a PostgreSQL statement already was; a
  stopped `execute` is rolled back and changes nothing. Before, a statement had no
  time limit.
- A tool with `once` is advertised as idempotent (`idempotentHint`) only when its key
  is kept for ever: a key that expires lets the same call run again.
- A tool granted a C function is warned about at start: C code cannot be
  interrupted, so a call that never returns holds its slot and reservation.
- The error when another process holds the journal directory now reads "another
  process is using ...", since that process may be an app's tools.

### Fixed

- `staal_run_status` returned a finished run's result whatever its size, so a result
  too large for the call's answer (over 1 MiB) could be had through it. It now gives
  the size and a reason instead, as the call does.
- A run's folder (`<journals>/<tool>/<shard>`) is flushed into its parent when made, so
  a power loss cannot take away a folder whose journal was already flushed.
- `docs/PACKAGES.md`: the Postmark library checked a success answer against a type
  that required `MessageID` before reading `ErrorCode`, so a refusal sent with status
  200 and no `MessageID` was reported as malformed rather than as Postmark's refusal.

## 0.24.1 — 2026-10-28

### Fixed

- A POST (or PUT, PATCH, DELETE) granted `idempotency_key` whose answer was lost was
  reported to the program as a failed action, which `try` could catch, while the
  service may have acted on it: the money moved and the run said it failed, and a
  program that caught the failure and sent the request again did so as a new
  operation, with a new key the service would not deduplicate. Now the run stops
  (`in_doubt`) and resuming sends the request again with the same key, as RUNTIME.md
  section 12 said it would. Found by the agent benchmark (`bench/agent`), where a lost
  Stripe answer left a refund made and its email unsent. A GET without an answer is
  still a failure the program can catch.

### Added

- `bench/agent`: the same support agent written in Python alone and in Python with
  Staal, run against prompt injection, malformed model output, permission escalation,
  failing services and crashes around every external call, and against realistic
  later changes to each agent; with its code size and what each team maintains.

## 0.24.0 — 2026-10-28

### Added

- Packages for Linux on arm64 (wheel, archive and npm addon, glibc 2.28 and later)
  and for Intel Macs (macOS 10.12 and later). Apple Silicon wheels now say macOS 11
  and later, as the binaries need, instead of the build machine's macOS 15. Each
  wheel's tag is checked against the minimum macOS its binaries declare. CI and
  release builds run on all four platforms.
- `@staal/node` carries addons for Linux x86_64 and arm64 and both Macs.
- A quickstart in the README and on the site (`examples/quickstart/`), run as written
  by the Python and Node tests.

- The `payments` package refunds through Paystack (`payments.paystack.refund`) and
  Flutterwave (`payments.flutterwave.refund`), beside Stripe. Each refund is checked
  against its transaction first (it exists, succeeded, and is in the refund's
  currency); Paystack is sent kobo, Flutterwave v3 naira, in whole units only. Tested
  against a local server answering as each service documents; not yet against the
  live services.

## 0.23.0 — 2026-10-27

### Added
- A Node.js and TypeScript package (`node/`, npm name `@staal/node`; npm refuses `staal` as too close to `stall` and `steal`): `run`, `resume`,
  `resolve` and `check` over the C interface, returning Promises and running off the
  event loop, with types for every result. Tested on both engines, through real
  crashes at every journal write (a child process killed, the run resumed and, where
  an action was left in doubt, resolved), installed from its packed file.
  `scripts/node_package.sh` packs it.
- Linux builds for glibc 2.28 and later without CI, through cargo-zigbuild: the
  wheel (`STAAL_RELEASE_DIR`, `AUDITWHEEL_PLAT` in `scripts/wheel.sh`) and the npm
  package (`STAAL_ZIGBUILD=1`). RELEASING.md says how, and how to publish to PyPI and
  npm.
- `.github/workflows/publish.yml`: publishing a release uploads its wheels to PyPI,
  through trusted publishing (no token stored), and its npm package to npm.
- GUIDE.md, "Policy from your app": an application works out each run's limits
  (a customer's tier, an order's cost) and grants them; Staal enforces them and the
  code's own limits. `examples/policy/` holds the program and a Python app, tested
  on both engines and through a resume (`python/tests/test_policy.py`).
- Release archives and wheels carry `LICENSE-THIRD-PARTY.txt`, the licences of every
  crate the binaries contain (`scripts/third_party_licenses.py`), with LICENSE and
  NOTICE.
- `install.sh`: installs the latest release for Linux x86_64 or macOS with one
  command, checking the archive's SHA-256, into `~/.staal` and `~/.local/bin`.
- PostgreSQL: `db.postgres.query` and `db.postgres.execute`. A grant names the
  databases it reaches (server, database, user, password from the environment);
  TLS is required and verified, except to a server on this machine. Queries run in a
  read-only transaction and give exact JSON rows; statements commit durably, with
  idempotency keys kept in the database when the grant asks. A statement whose
  connection breaks during COMMIT is left in doubt, never guessed. Tested against a
  real server, with crash tests at every point. MongoDB is deferred.
- A program that needs no grants runs without a config file:
  `staalc run prog.staal [--entry <function>] [--input <name>=<value>]...` (`main`
  by default; Text inputs as written, others as JSON), for `run`, `resume`, `profile`
  and `debug`. Capabilities and Secrets still come only from a config file.
- `staalc config prog.staal [--entry <function>]` prints a run config to fill in:
  placeholders of the right type for every input, grants with the limits the code
  declares (and what library capabilities need), secrets, and a model when the
  function can ask one.
- `"model"` may be left out of a run config; a model call then fails, saying so,
  before anything is sent or recorded.
- Licensed under the Apache License 2.0 with the LLVM Exceptions (LICENSE, NOTICE).
- docs/TUTORIAL.md, "Staal in one hour", checked by a test that follows it in an
  empty folder and compares every command's output.
- CONTRIBUTING.md and CODE_OF_CONDUCT.md (Contributor Covenant 2.1).
- Fuzzers for program text, the formatter, journals, run configs and package files
  (fuzz/, `cargo fuzz` on stable Rust).
- C libraries: `foreign ffi.<library> { fn ... }` declares C functions with C types
  (integers, `bool`, `text`, at most six arguments), each the capability
  `ffi.<library>.<function>`. A grant names the library file and its SHA-256, checked
  before the run starts; on Linux the library is loaded through the descriptor that
  was hashed. Every run granted one is marked unchecked: an `unchecked` event in the
  log, a warning from `staalc run`, `"unchecked"` in the C interface's result, and a
  section in `staalc doc`. Arguments are checked against the declaration before the
  call; calls are journaled, replayed, made one at a time per library, and a call cut
  short by a crash is made again only when the grant says `"repeatable": true`.
- Version ranges for dependencies: `{"git": ..., "version": "^1.2"}` takes the newest
  release tag of that repository within the range, and `{"version": "~0.3"}` the
  newest version the project's registry lists. Ranges are Semantic Versioning
  (`^`, `~`, `=`, comparisons, wildcards; pre-releases only when named). One version
  per package fits every range asking for it; a locked one is kept while it fits, and
  `staalc update [--package <name>]` moves it. A conflict is reported with who wants
  what, never searched around. A moved release tag or registry entry, or a tag whose
  manifest gives another version, stops the fetch with nothing changed. `staal.lock`
  is version 2 (version 1 is still read), recording each package's version.
- Package registries: a git repository listing each published version's commit and
  the SHA-256 of its files, checked on every fetch. `staalc publish` adds a version
  (a published version never changes); `staalc yank` stops one being chosen again.
- `net.tcp.accept` and `net.tcp.reply`: listening. `accept` waits for the next client
  on an address `only` allows and gives its request as an `Incoming`; `reply` answers
  that connection and closes it. Tasks made by `parallel` serve clients at once. The
  request is untrusted. Both are made again after a crash and never twice; a request
  accepted but not answered when a run stopped is lost with its connection, and the
  resumed run's reply fails saying so. A crash test checks it at every crash point.
  Plain TCP, not encrypted.
- `net.tls.exchange`: the TCP exchange over TLS, its certificate always verified
  against Mozilla's roots and the operator's extra ones and against the host. A failed
  handshake is `not sent`; a reply the peer did not end with close_notify is refused
  as possibly cut short.
- `parallel(list, f, ...)`: concurrency. `f`, a declared function, runs for every item
  at once on up to 16 threads; the results come back in the list's order, and a
  failure is the first by position, so the outcome never depends on scheduling. Tasks
  share no data; a capability limited by `uses` or `max` is refused for them. Each
  task's operations are their own stream in the journal (version 2), so a resumed run
  replays every task exactly: the crash tests kill concurrent actions and model calls
  at every crash point and resume on either engine. `staalc resolve --operation`
  names one of several actions in doubt. Budgets count every task (E004); logs are
  the same whatever the scheduling. The debugger and the REPL run tasks in turn.
- Scripted model replies can say `"when"`, the text of the request they answer.
- `net.tcp.exchange`: a TCP request and its reply, read to the peer's close or to a
  delimiter, to the addresses `only` allows (canonical `host:port`, `host:*` or `*`).
  An exchange that may have reached the peer is never repeated: it is left for an
  operator, who records the reply with the new `staalc resolve --reply <text>`. A
  crash test checks it at every crash point. Plain TCP, not encrypted.
- `db.sqlite.query` and `db.sqlite.execute`: SQLite databases as runtime
  capabilities. A query opens the database read-only and gives its rows as JSON for
  `parse`; a statement runs in a durable transaction. SQL must be trusted (E006);
  values from outside are bound parameters. Paths are checked as `fs` checks them.
  Queries replay from the journal; a statement left in doubt stops for an operator,
  or with `"idempotency_key": true` is resolved by a key kept in the database in the
  same transaction, which the crash tests check at every crash point. Linux only.
- `read_line()`: console input, `some` of the next line or `none` at the end. It is
  journaled (`input`): a resumed run replays the lines it recorded and reads only the
  rest, which a crash test checks at every crash point. Its text is untrusted.
- Releases: `scripts/package.sh` (a release archive with its SHA-256),
  `scripts/wheel.sh` (a Python wheel holding `staalc` and `libstaalc`, so `pip
  install` needs no Rust) and `scripts/wheel_test.sh` (the wheel installed into a
  clean environment and used with no Rust on PATH). A tag `vX.Y.Z` runs the release
  workflow on Linux and both macOS kinds; CI builds and tests the wheel on every
  push. `staalc --version`. `docs/RELEASING.md` says how to release; a test keeps
  the version the same in staalc, the Python package, the VS Code extension and the
  changelog. `staal.Library()` finds the library installed with the package.
- `staalc debug`: breakpoints by line (in the program or a module), step, next,
  finish, and variables shown as JSON, on a run that is otherwise `staalc run`'s,
  on the interpreter.
- `staalc profile`: per function, calls, steps and time (total and own), and the
  model calls, tokens and actions it made, from a run that is otherwise `staalc
  run`'s. The counts are the same on every run, and the steps on both engines.
- `staalc repl`: declarations, statements and expressions one at a time, each checked
  and run as a whole program by the same checker and runtime as a file, on both
  engines. Inputs that read the clock, draw a random number or call a model are not
  kept, and nothing reaches the world.
- `staalc doc [-o <out.md>]`, generated documentation: each declaration as written
  (signatures without bodies, a capability's `needs` and action signatures) with the
  `//` block above it. Only a program that passes every check is documented.
- `staalc test`, for tests written in Staal: every `test_` function of a test file
  runs as its own run in both engines, which must agree on the outcome and the log.
  `tests/staal` tests the standard library and the language this way, and `cargo
  test` runs it in each engine.
- A VS Code extension in `editors/vscode`: a TextMate grammar and a client for
  `staalc lsp`. Its tests run the grammar with VS Code's tokenizer over every Staal
  file in the repository and the server with VS Code's protocol library; a cargo
  test keeps its reserved words and built-ins identical to the compiler's.
- `staalc lsp`, a language server: errors as you type, completion and formatting,
  for any editor with an LSP client. Its errors come from the same function as
  `staalc check`, and the tests require them to be identical for every program in
  the repository.
- `staalc fmt [--check] <file>...`, the formatter. It changes only the whitespace
  between tokens and keeps every comment and line break, so it cannot change what a
  program means; it checks its own result before writing. The examples and packages
  are formatted with it.
- `docs/GRAMMAR.md`: the complete syntax of Staal. A test keeps it exact: its
  reserved words, contextual words and built-in functions are the compiler's, and
  every example in it parses.
- `print`, `now` and `random_int`: printing to standard output, the current time,
  and uniform random Ints from the operating system's random source. Each is an
  external operation in the journal (`print`, `clock`, `random`), so a run resumed
  after a crash at any point uses exactly the values it had already given the world,
  and prints each line once; a crash test proves it at every crash point. They run on
  the interpreter only, and a function value cannot use them.
- Dates and times, on both engines: a time is an Int of milliseconds since
  1970-01-01T00:00:00Z (UTC), for the years 0000 to 9999. `format_time`,
  `parse_time` (strict RFC 3339, an optional value), `make_time`, `date_parts`,
  `time_parts` and `weekday`. Every day of the range is tested to round-trip.
- Hashing and encoding, on both engines: `sha256` (lowercase hex), `base64_encode`
  and `base64_decode` (standard alphabet, padded, strict), `hex_encode` and
  `hex_decode`. The decoders give an optional value. Checked against the published
  SHA-256 and RFC 4648 test vectors. A hash of blinded data still reveals it (E007).
- `map`, `filter`, `count`, `any`, `all` and `fold`: list functions that call a
  function value on each item in order, on both engines; native code calls the
  compiled function directly. `any` and `all` stop at the first item that decides.
- The standard library's text, number and list functions, on both engines: `upper`,
  `slice`, `index_of`, `split`, `join` and `to_int`; `abs`, `min`, `max` and `rem`;
  `append`, `concat`, `reverse`, `sort` and `sum`. Lists keep their maximum size:
  `append` fails on a full list, `split` takes the most parts in the call, and
  `concat`'s result has room for both. Positions count characters.
- Function values, of type `fn(Int) -> Int`: a named function used as a value,
  or `fn(x: Int) -> Int { return x + k }` written in place, which copies the
  variables it reads. They are passed, returned, held in variables and called as
  `f(x)` or `adder(3)(4)`, and generic functions take them. A function value only
  computes (no action, model call, `done`, printing, clock or random number, through
  any call), so no capability,
  secret or proof ever enters one: reading a capability is E003. They are never
  kept inside data, compared, encoded or sent. The compiler lifts each one written
  in place into a function of its own; both engines run them, and native code
  calls them natively.
- Interfaces, `interface Priced { fn price(self) -> Money }`, as bounds on type
  parameters, `<T: Priced + Named>`. A type satisfies one by having its methods,
  with the same parameters, result, `fails` and trust, which the caller could call
  itself; calls through a bound resolve in each copy to the type's own method.
- Generic functions, `fn first<T>(xs: List<T, 10>) -> Optional<T>`. Type
  parameters are inferred from the arguments; the body is checked with each as an
  unknown type, and each set of types used becomes a copy (`first<Text>`) that is
  checked and run like any function, on both engines.
- Methods, `fn Line.total(self) -> Money`, declared beside a record or enum and
  called as `line.total()` on any expression, with named arguments and defaults.
  The compiler resolves each call by type and rewrites it into a plain call before
  checking, so every rule applies unchanged. On a capability, `x.m()` is always an
  action.
- Typed errors: `fn f(...) -> T fails E` with E a record or enum, `fail(e)` with a
  value of E, and `outcome.failure`, an `Optional<E>`, after `try f(...)`. A
  declared failure that is not caught must be declared by the caller too, so
  `.failure` always holds the declared type. Both engines carry them.
- Maps, `Map<Text, Int, 100>`, and sets, `Set<Text, 100>`, bounded like lists,
  with Text or Int keys kept in order. `["a": 1]` and `[:]` build maps; `get`,
  `put`, `remove`, `keys`, `values`, `entries`, `add`, `items` and `to_set` work on
  them, and `len` and `contains` take them. `put` and `add` fail past the maximum
  size. In JSON a map is an object and a set an array of distinct items. A model is
  never asked for one. `for (k, v) in entries(m)` unpacks any list of tuples.
- On the native engine, lists, maps and sets may go where a larger one is allowed,
  and empty typed literals compile, so more functions run as native code.
- Tuples, `(Int, Text)`, built as `(1, "a")`, read as `t.0` and unpacked with
  `let (n, s) = t`. In JSON a tuple is an array of exactly its length. A model is
  never asked for one. Blind paths name positions (`@blind(pair.1)`). Both engines
  run them.
- Default values, `fn f(x: Int, tries: Int = 3)`, and named arguments,
  `f(1, tries: 5)`. Defaults are constants; arguments are evaluated in the order
  written and passed to the parameters they name, on both engines. An entry
  parameter with a default may be left out of a run's `inputs`.
- Type aliases, `type Amount = Money`: another name for any type, replaced by that
  type before checking, so the two are interchangeable everywhere. Aliases of
  records and enums build their records and variants. Cycles, clashing names and
  aliases of unknown types are errors; `pub type` exports one.
- Optional values, `Optional<T>`, with `some(x)` and `none`, matched with
  `some(x)`, `none` and `_`. `none` takes its type from where it is used; a T is
  never made optional implicitly. In JSON `none` is `null`, and a model is asked
  for one through an `anyOf` with `null`. Blinding reaches through them: whether
  a blinded value holds anything decides nothing (E007). `some` and `none` are
  reserved names. Both engines and `staalc build` executables run them.
- Enums, `enum Payment { Pending, Paid { amount: Money, reference: Text } }`,
  and `match` with field patterns, `..` and `_`. A `match` must handle every
  variant (new code E109). Enums are read from and written as JSON objects with
  a `kind`, so a model's answer is validated into a declared variant. Choosing an
  arm reveals only the variant (E007), and bound fields keep the value's trust
  (E006). Both engines and `staalc build` executables run them, with the same
  results, failures and step counts.
- Module-level constants, `const NAME: T = value`, computed when the program is
  checked and substituted where used; `pub const` exports one.
- `continue` in `repeat`, `loop` and `for`, on both engines. Skipping the rest
  of an iteration on a blinded value counts as deciding what follows (E007).
- Multi-line text between `"""` quotes, with the closing quotes' indentation
  removed.
- Text interpolation, `"\(expr)"`, for Text, Int, Bool and Money; it is joining
  text with `+`, so the trust and blinding rules apply unchanged. `to_text(x)`
  does the conversion directly.
- Escape sequences in text: `\"`, `\\`, `\n`, `\t`, `\r` and `\u{...}`. An
  unknown escape is an error (E900).
- Claude as a model provider (`"provider": "anthropic"`), through Anthropic's
  Messages API with structured outputs. Tokens are counted before each call and
  charged from the usage the API reports; rate limits and overloads are retried.
- A run whose model provider stays unavailable stops (exit code 3) instead of
  failing, and resuming makes the call again.
- Refusals and output cut off at the token limit are `provider` failures, and
  their tokens are charged.
- `examples/claude/triage.staal`, run against the live API: the injected request
  for a larger refund stays data, and the token count matched the usage reported.
- `extra_root_certificates` for the Claude provider, as for `net.http`.

### Changed
- A `Response` (and the new `Incoming`) can no longer be proposed by a model or
  parsed from text, as they could not be built in code (E101).
- Any expression followed by arguments is a call of the function value it gives:
  `f(1)(2)` is one call of what `f(1)` returns, where before the `(2)` was read as
  a separate statement.
- A variable that has the name of a built-in function (`add`, `get`, ...) cannot be
  called: `add(x)` is the built-in, so with such a variable in scope it is E101.
- `pub capability` and a mark other than `@blind` on a method's `self` are syntax
  errors (E901). Both were accepted and silently had no effect.
- The standard library's names are reserved: a function named `split`, `sum`,
  `min`, `max`, `map`, `any`, `all`, `count` or any other of them must be renamed.

### Fixed
- macOS: a `net.tcp.exchange` whose peer answered and closed at once was reported
  as in doubt, because macOS refuses a new socket timeout once the peer has closed.
  The read timeout is now set before sending and kept when an update is refused;
  `net.tcp.accept` the same. Found by CI on its first macOS run, with three test
  problems: a TLS test server that could reset the connection, a PostgreSQL test
  server reachable on 127.0.0.1 only while macOS resolves localhost to ::1, and the
  licences of two macOS-only crates (objc2), which `release_test` now checks for every
  release platform.
- A package could be named `db` or `ffi`, and a library capability declared under
  `db.`, capability families of the runtime; all are refused now, as `net` and `fs`
  were.
- `staalc resolve` refuses an operation that resume makes again by itself (`fs.read`,
  `fs.write`, `db.sqlite.query`, `net.tcp.accept`, `net.tcp.reply`). Before, it could
  record a receipt for an `fs.read` or a `db.sqlite.query`, and the resumed run then
  failed on a result it could not read.
- `put` and `add` were not counted as operations that can fail, so whether code
  after a full map or set ran was not tracked as depending on their inputs (E007).

## 0.22.0 — 2026-10-19

The first version published in this repository. Earlier versions (0.1 to 0.21)
were internal; their history is in the git log.

### Language and compiler
- Eight safety rules checked at compile time (E001 to E008): actions need
  capabilities, capabilities cannot be widened, model output must be validated,
  untrusted text cannot instruct a model, model use stays within declared budgets
  and out of unbounded loops, blinded data cannot reach a model, and completion
  needs evidence.
- Exact money with currencies, bounded lists, records, `try`, modules with `pub`
  and imports, and reassignment checked for order.
- Capabilities declared by libraries, with `canonical` targets and limits
  (`only`, `uses`, `each`, `max`) enforced before a library's code runs.

### Runtime
- Reference interpreter with a capability broker and model budgets.
- Crash-safe journal: every external effect is recorded before and after it
  happens; runs resume after a crash, and an action whose outcome is unknown is
  left for an operator to resolve.
- `net.http` (HTTPS through rustls) and `fs` capabilities, with secrets taken only
  from environment variables.

### Packages
- Dependencies by git URL and exact commit, fetched without a checkout and locked
  by the SHA-256 of every file in `staal.lock`.
- A package's manifest declares its authority, and code that reaches beyond it is
  refused (E108).

### Native code and interfaces
- Pure functions compile to machine code through Cranelift, with results, errors
  and journals identical to the interpreter.
- `staalc build` links a program into a standalone executable (Linux).
- A C interface (`include/staal.h`) and a Python wrapper over it.

### Changed
- The language is renamed from Intent to Staal: the compiler is `staalc`, source
  files end in `.staal`, and projects use `staal.json` and `staal.lock`.
