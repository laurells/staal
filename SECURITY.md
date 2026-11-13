# Security policy

Staal's compiler and runtime make guarantees about what a program may do
([docs/GUARANTEES.md](docs/GUARANTEES.md)), so a flaw that lets a program get past
one is a security problem, not only a bug.

## Supported versions

Staal is in preview. Only the latest release gets security fixes; an older release
is fixed by upgrading.

| Version | Supported |
| ------- | --------- |
| 0.29.x  | Yes       |
| < 0.29  | No        |

## Reporting a vulnerability

Never report a vulnerability in a public issue, pull request or discussion. Use
GitHub's private vulnerability reporting on this repository: open the "Security"
tab, then "Report a vulnerability". If that is unavailable, email
laurels.echichinwojr@gmail.com with "Staal security" in the subject.

Include the version (`staalc --version`), the platform, and the smallest program,
run config and commands that show the problem, with what you expected and what
happened.

## What to expect

You will get an acknowledgement within 7 days and an assessment (accepted or
declined, with the reason) within 30 days. While a fix is in progress you will be
told of progress at least every 30 days. Accepted problems are fixed in a new
release, announced in CHANGELOG.md and a GitHub security advisory, with credit to
you unless you ask otherwise. Please keep the problem private until the fix is
released, or until 90 days after your report, whichever is sooner.

Staal is maintained by one person. These times are the commitment; most reports
will be answered sooner.

## In scope

Anything that breaks a guarantee Staal makes, on either engine, for example:

- the compiler accepting a program that breaks one of the safety rules (E001 to E008)
- a program acting without a capability, or beyond its limits (`only`, `uses`,
  `each`, `max`), or a package reaching beyond the authority its manifest declares
- a resumed run repeating an action the journal recorded as done, or guessing the
  outcome of one in doubt
- a `Secret` reaching the journal, the log, a model's input or an error message
- the interpreter and native code disagreeing in a way a program can exploit
- a crash, hang or memory error in `staalc` or `libstaalc` caused by a source file,
  run config, journal, manifest, lock file or package
- a fetched package or registry entry whose contents differ from its recorded hash
  being accepted
- `install.sh` installing a file whose checksum does not match
- for tools (`staalc mcp`, and `staal.Tool` or `Tool.open` in an app): anything in
  [Tools: what is trusted](#tools-what-is-trusted) below that does not hold

## Tools: what is trusted

`staalc mcp` serves programs to an MCP client, and an app's tools serve them to an
agent framework, through the same host (docs/MCP.md, sections 8 and 11). The client,
the model and everything it reads are untrusted: a prompt injection can make the model
call any tool with any arguments, as often as it likes. These hold whatever it sends,
and a way around any of them is in scope:

- **Arguments never become authority.** They are the entry function's inputs, checked
  against its types; nothing in them widens, chooses or fills in a grant. Grants come
  from the manifest, or from the app's own code for each call, never from the call.
- **Limits hold across calls.** A quota (calls and money per window) is checked and the
  run's worst-case spend reserved, in a ledger flushed to disk, before the run starts;
  concurrent calls cannot overspend together, and a crash or restart does not reset
  it. A `once` key makes a repeated call the same run, never a second one.
- **Nothing is done twice, and nothing in doubt is guessed.** A run a crash interrupted
  is resumed from its journal with the program and config it started with; one whose
  action may have happened waits for an operator, and its resolution never comes over
  MCP or from the model.
- **Secrets stay out.** No secret reaches the client, the model, a log line, the
  ledger, a run file, a journal, a waiting record, or the notify command, which runs
  with a cleared environment and none of the host's descriptors beyond its standard
  ones. (C code given a secret can print it; that is the operator's grant.)
- **No call holds the host without bound**, except through C code. A tool may
  listen for connections only with a `wait_ms` limit on each accept, every network wait and every SQLite statement has a
  timeout, a tool's `deadline` stops a run starting new actions, and the concurrency
  limit refuses calls beyond it rather than queueing them. A C function (`cap ffi`)
  cannot be interrupted; a tool granted one is warned about at start.
- **Tenants stay apart.** In an app serving several tenants or users from one
  directory, a call's trusted scope comes from the app, never from the arguments; a
  `once` key is held within its scope, and a run's status is given only in its scope.
- **One process owns a journal directory**, by an OS lock, so two processes cannot
  both act on one ledger.

A forward jump of the clock while a host runs moves no quota or `once` key on; across a
restart it can, which the host warns about (docs/MCP.md section 6). What these do not
cover: a model choosing a harmful action that the grants allow (the
grants are the operator's statement of what is allowed), a client showing a tool's
result to a model as instructions (results are data; treat them as untrusted), and
anyone who can write to the journal directory, which must belong to the host's user.

## The MCP server over HTTP

`staalc mcp` can also serve tools over Streamable HTTP with OAuth access tokens
(docs/MCP.md section 12). A release build refuses it until it has had an independent
security review; it runs only in builds made with `--features failpoints`. Reports
against it are welcome and in scope now: anything in docs/MCP.md section 12 that does
not hold (a token accepted that should not be, a request served before its checks, one
principal reaching another's runs, quota or answers, a token, key or subject reaching a
log or file).

## Severity

Accepted reports are rated with CVSS v4.0 (https://www.first.org/cvss/v4-0/),, and the
vector is published in the advisory.

## Out of scope

- Runs that load a C library (`foreign`, `cap ffi`): Staal cannot check C code, and
  marks those runs unchecked. A flaw in Staal's own loading or hashing of the
  library is in scope.
- Problems in the services a program talks to, or in a model's answers, that Staal
  handles as documented.
- What an operator grants: a run config that grants broad authority is the
  operator's decision.
- Problems that need an attacker who can already change the program, its run config
  or its journal on disk.
