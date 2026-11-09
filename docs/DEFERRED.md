# Deferred work

Work decided on and put off, with what finishing it means. An item leaves this file
when it is done (and goes to CHANGELOG.md), not when it is started.

## Listening (`net.tcp.accept`, `net.tcp.reply`)

Deferred on 2026-10-26, after listening shipped (commit e771d09), to start phase 12.

### Not fixable: a request lost to a crash

A connection does not outlive the process that accepted it; no runtime can resume
one. The runtime never answers twice and says plainly when a request was lost
(RUNTIME.md section 12). The remedy is in the protocol: clients retry, with an
idempotency key. If wanted, document that pattern with an example; there is nothing
to fix in the runtime.

## The MCP server (`staalc mcp`)

Deferred on 2026-11-01, when version 1 shipped (docs/MCP.md).

### Progress per operation

A call sends one `notifications/progress`, when its run starts. Sending one at the
start of each action and model call needs an event hook in the interpreter, which
runs on both engines and inside concurrent tasks. Needed: a hook that cannot change
what a run does (it only observes), called from the same place the run's log is
written, on both engines. Done when: a test sees one progress message per external
operation in order, on both engines and with tasks, and the engines' logs and
journals are unchanged.

### Remote transport: the release gate

Streamable HTTP with OAuth is built and tested (docs/MCP.md section 12), and served
only by a test build. Done when: an independent security review
(docs/security-review/) has been made and its findings fixed; then a release build
may serve `server.http`.

## Packages

Deferred on 2026-11-02.

### Microsoft Teams (`chat.teams`)

Posting to a Teams channel goes through Microsoft Graph, whose access tokens are
fetched from Microsoft's identity platform and expire. A package's `Secret` need is a
fixed value from the environment, so a package cannot fetch or refresh one, and a token
put in the environment would stop working within hours. Needed: a way for a library
to obtain a token that is never itself a value the program can read, journaled so a
resumed run does not fetch it twice. Done when: designed in RUNTIME.md, then the
package is written from Microsoft's documentation with tests like the others.

## MongoDB

Deferred on 2026-10-26, when PostgreSQL was built (`db.postgres`). Needed: `db.mongo`
capabilities as the runtime's own, documents coming back as JSON for `parse` and
`validate`. Two things to settle first. The Rust driver is asynchronous; it would run
on its own small runtime, as the Postgres client does. And crash safety: a write and
its idempotency key can be kept together only in a transaction, which MongoDB has only
on a replica set or sharded cluster; on a single server a write left in doubt must stop
the run for an operator, and the docs must say so. Done when: tests against a real
server (a replica set for the keyed path), crash tests at every point with and without
keys, mutations caught, and documented.

## Phase 10 (platforms and distribution), partly done

Done (2026-10-28): CI tests, and release.yml builds and tests the wheel, archive and
npm addon on, Linux x86_64 and arm64 (manylinux_2_28) and macOS on Apple Silicon
(11+) and Intel (10.12+); PyPI and npm publish through trusted publishing. The C library tests
(`cap ffi`) run in CI on all four. Remaining:
Windows builds and tests, Linux with musl (index.js already looks for a
`linux-<arch>-musl` addon, none is built), cross-compiling, wasm, and `staalc build`
executables on macOS.

The CI badge was taken off the top of README.md on 2026-10-27, because it showed
failing runs. CI now passes on all four platforms; put it back if wanted:
`[![ci](https://github.com/laurells/staal/actions/workflows/ci.yml/badge.svg)](https://github.com/laurells/staal/actions/workflows/ci.yml)`

## Reliability, to do before more features

CI tests on four platforms, and six fuzz targets exist (fuzz/README.md). Not yet
done: a security review by someone other than the author (the package for it is
docs/security-review/), fuzzing with AddressSanitizer and in CI, fuzzing the network
code, and load and long-running tests (many connections, hours of running, large
journals).

`packages_test` failed once under load (`run_config_durable` Resume unwrap at
tests/packages_test.rs:452) and has not failed in 18+ runs since. Cause unknown:
investigate if it fails again.
