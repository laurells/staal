# Security policy

Staal's purpose is to make programs that act on the world safe, so a flaw that lets
a program get past its guarantees is a security problem, not only a bug.

## Supported versions

Staal is in preview. Only the latest release gets security fixes; an older release
is fixed by upgrading.

| Version | Supported |
| ------- | --------- |
| 0.23.x  | Yes       |
| < 0.23  | No        |

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
