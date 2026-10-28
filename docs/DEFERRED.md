# Deferred work

Work decided on and put off, with what finishing it means. An item leaves this file
when it is done (and goes to CHANGELOG.md), not when it is started.

## Listening (`net.tcp.accept`, `net.tcp.reply`)

Deferred on 2026-10-26, after listening shipped (commit e771d09), to start phase 12.

### TLS when listening

`net.tcp.accept` is plain TCP: nothing is encrypted. Needed: a `net.tls.accept`
whose certificate chain and private key come from the operator's run config, the key
held as a `Secret` (never in the journal, the log, a model's input or an error
message), with an expired or mismatched certificate and key refused when the run
starts, not at the first client. Settle how a certificate is replaced in a long run
(at the next run, or on a signal). Done when: tests against a TLS client verifying
the certificate, a fixture or config error for every bad key and certificate case,
the crash test for listening run over TLS too, mutations caught, RUNTIME.md and
GUIDE.md updated.

### An optional time limit on waiting for a client

Waiting for a client has no time limit, deliberately: a server waits. Some programs
want "at most N seconds". Needed: an optional limit (on the grant or as an argument;
decide which), failing as `action` when it passes with no client, and replayed from
the journal like any accept. Done when: tested on both engines, with a crash test
point inside the wait, and documented.

### Skipping bad clients by default

Today a client that is too slow, sends more than `net.max_response_bytes`, or sends
something that is not UTF-8 fails the whole `accept`, so a server must wrap each one
in `try`. For a server that is routine, not a program error. Needed: by default the
runtime drops that client, logs it (an event, with the reason, never the request),
and keeps waiting; a program can still ask for the strict behaviour. Done when: a
test with bad clients between good ones shows every good one answered and every bad
one logged, on both engines, and documented.

### Not fixable: a request lost to a crash

A connection does not outlive the process that accepted it; no runtime can resume
one. The runtime never answers twice and says plainly when a request was lost
(RUNTIME.md section 12). The remedy is in the protocol: clients retry, with an
idempotency key. If wanted, document that pattern with an example; there is nothing
to fix in the runtime.

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

Everything has been tested on one Linux machine only. Not yet done: fuzzing (first
the parser and the journal reader, which take input from outside; then the checker,
the network code and the config reader), a security review by someone other than
the author, and load and long-running tests (many connections, hours of running,
large journals). Planned for phase 13; proposed to come earlier.

`packages_test` failed once under load (`run_config_durable` Resume unwrap at
tests/packages_test.rs:452) and has not failed in 18+ runs since. Cause unknown:
investigate if it fails again.
