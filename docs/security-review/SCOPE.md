# Scope of the review

## 1. Target

The `staalc` repository at the commit named in [README.md](README.md): the language,
its compiler, both execution engines, the runtime, the tool host and MCP server
(stdio and HTTP), the C interface and its Python and Node.js bindings, packages,
and the release process. The reviewed commit is release 0.27.0. It contains the HTTP
transport, which its release builds refuse to serve until this review is done.

## 2. In scope

Ordered by priority: what a flaw there would cost and how directly an attacker
reaches it.

| Priority | Area | Code | Why |
| --- | --- | --- | --- |
| 1 | **MCP over HTTP with OAuth** | `src/mcp/http.rs`, `jwt.rs`, `keys.rs`, `principal.rs` | network-facing; not released until this review passes (MCP.md 12.7) |
| 1 | **The broker and capability checks** | `src/interp/broker.rs`, `src/targets.rs`, `src/interp/money.rs` | every action passes here; a flaw is a way past the operator's limits |
| 1 | **The checker's safety rules** (E001 to E008, E108) | `src/checker.rs`, `src/budget.rs`, `src/calls.rs` | the language's central claim: model output never becomes authority |
| 2 | **Journal and recovery** | `src/interp/journal.rs`, `src/host/` | exactly-once: a flaw repeats a payment or a message |
| 2 | **Tool host**: quotas, `once`, scopes, ledger | `src/host/` | concurrency and crash safety across calls |
| 2 | **Runtime capabilities**: HTTP, TCP/TLS, files, databases | `src/interp/http.rs`, `tcp.rs`, `fsx.rs`, `db.rs`, `pg.rs` | parse what services send; file access against links and races |
| 2 | **Secrets handling** | `src/interp/mod.rs`, `http.rs`, the host | secrets must not reach a journal, log, model or client |
| 2 | **Native engine** | `src/native.rs` | most of the `unsafe` code ([UNSAFE.md](UNSAFE.md)) |
| 3 | **C interface and bindings** | `src/capi.rs`, `include/staal.h`, `python/staal/`, `node/src/lib.rs` | memory ownership across a language boundary |
| 3 | **Packages and registries** | `src/packages.rs`, `src/semver.rs` | code from other people; `git` invocations |
| 3 | **MCP over stdio** | `src/mcp/mod.rs`, `proto.rs`, `manifest.rs` | local client; mostly shared with HTTP |
| 3 | **Distribution** | `.github/workflows/`, `install.sh`, `scripts/` | what users install |
| 4 | **Developer tooling**: REPL, LSP, formatter, docs, debugger | `src/repl.rs`, `lsp.rs`, `fmt.rs`, `doc.rs`, `debug.rs` | local, run by the developer on their own code |
| 4 | **Library packages** | `packages/` (Staal) | run under the same broker as any program |

## 3. Out of scope

- C code a program loads through `cap ffi` (Staal's loading and hashing of it is in
  scope), and the services programs talk to.
- The OAuth authorization server, the MCP clients, and model providers.
- The VS Code extension (`editors/vscode`): it starts `staalc lsp` and adds syntax
  highlighting.
- Example programs (`examples/`) except as illustrations; the documentation site.
- Anything that needs a trusted party to act against itself (the operator, the app,
  the machine: THREAT_MODEL.md section 2), and
  anything in [LIMITATIONS.md](LIMITATIONS.md) sections 1 and 2, unless the reviewer
  thinks it should not be accepted.

## 4. Standard

The owner and reviewer choose the verification level. Proposed: OWASP ASVS 5.0.0
Level 2 for the HTTP transport (chapters V4, V8, V9, V10, V12, V16), and a
source review against the threat model for the rest. [COMPLIANCE.md](COMPLIANCE.md)
maps each ASVS chapter to the code.

## 5. Questions for the reviewer

Where the author is least sure, and where an outside eye is worth most.

- **Q1. Is the checker sound for E001, E003 and E007?** Can a program, however
  contrived, get a `Proposed<T>` value to an action without `validate`, widen a
  capability, or move blinded data to a model through a path the flow analysis
  misses (closures, `parallel`, modules, packages, pattern matching, error values)?
  The rules are in [GUIDE.md](../GUIDE.md) "What the compiler enforces"; the 628
  failing fixtures in `tests/fixtures/fail` are the attempts so far.
- **Q2. Can the broker be got past?** By a target that canonicalises differently
  from what the library contacts, by an amount in another currency, by a capability
  narrowed then widened through a package, by concurrency.
- **Q3. Can recovery repeat an action?** Under any crash point, any journal edit the
  journal's own checks accept, or two processes.
- **Q4. Does the native engine's contract hold?** Can generated code pass a pointer,
  handle or argument count the helpers do not expect ([UNSAFE.md](UNSAFE.md)
  section 2)?
- **Q5. Is token verification complete?** Any token not issued by the configured
  issuer for this resource, unexpired and unrevoked-key-signed, that `jwt.rs`
  accepts; any way discovery can be pointed elsewhere.
- **Q6. Does the HTTP transport meet MCP 2026-07-28** as [COMPLIANCE.md](COMPLIANCE.md)
  sections 1 and 2 say? These were written by the implementation's author from the
  specification.
- **Q7. Can one principal affect another** beyond the shared limits by design: read
  its runs, take its `once` answers, spend its share?
- **Q8. Can a secret leak** through any path not covered by T26 and T49 (an error
  message, a database error, a TLS alert, a log of a refused request)?
- **Q9. Are the accepted risks acceptable?** [LIMITATIONS.md](LIMITATIONS.md),
  particularly no token revocation (up to the token's lifetime) and the 24-hour key
  window.

## 6. Logistics

- Contact and disclosure: [SECURITY.md](../../SECURITY.md). Findings are reported
  privately, through GitHub's private vulnerability reporting.
- Building and running everything: [BUILD.md](BUILD.md).
