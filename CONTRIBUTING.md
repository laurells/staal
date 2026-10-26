# Contributing to Staal

Thank you for helping. Staal is meant to be relied on by programs that move money
and send messages, so every change is held to the same standard as the code already
here. This page says what that standard is, and how to meet it.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Before you start

For anything larger than a fix to a few lines, open an issue first and say what
you want to change and why. A change to the language (its syntax, a rule of the
checker, what a program may do) is a design decision, and is easier to agree on
before it is written. Bugs, unclear errors and gaps in the documentation are always
welcome as issues, with the smallest program that shows them.

Security problems are never reported in a public issue: use GitHub's private
vulnerability reporting on this repository ("Security", then "Report a
vulnerability").

## Building and testing

You need Rust 1.89 or newer, `git`, a C compiler (`cc`) and, for the editor
extension, Node.js.

```sh
cargo build
cargo test --features failpoints        # every test, with crash injection
cargo test --no-default-features         # the build without native code
cargo clippy --all-targets --features failpoints -- -D warnings
(cd editors/vscode && npm ci && npm test)
```

CI runs exactly these on Linux and macOS, with Rust 1.97.0. A change is ready
when all of them pass.

`fuzz/` holds the fuzzers (see [fuzz/README.md](fuzz/README.md)). Run the one for
the part you changed for a few minutes before sending a change to the parser,
checker, formatter, journal, run config or package files.

## What a change needs

These are the rules the existing code was built by. A change that does not meet
them is not finished, however well it works.

- **Both engines.** Staal runs programs on an interpreter and as native code. A
  change to what a program does must work on both and give identical results, logs
  and journals. Tests run on both (`engines()` in the integration tests).
- **Tests that would catch it breaking.** Every new behaviour has a test, and every
  new rule of the checker has an invalid program in `tests/fixtures/fail` (named
  `eNNN_...`, starting with `// expect: CODE` and a `// message:` line) and, where
  it allows something new, a valid one in `tests/fixtures/pass`.
- **Proof the tests catch it.** Break your change on purpose (remove a check, invert
  a condition, return early) and run the tests: they must fail. If one does not,
  the tests are not finished. Say in the pull request which breaks you tried.
- **Crash safety.** Anything that acts on the world goes through the journal and has
  a crash test (`tests/crash_test.rs`) that kills the run at every journal write and
  checks the resumed run ends exactly as an uninterrupted one, with nothing done
  twice. An action whose outcome is unknown is never guessed.
- **Failing closed.** When something is uncertain (a value from outside, a file that
  might have changed, an answer that might be cut short), the code refuses, with a
  message that says what was refused and what to do.
- **Documentation in the same change.** What a program or an operator can see
  changes the docs: [GUIDE.md](docs/GUIDE.md) for programmers,
  [RUNTIME.md](docs/RUNTIME.md) for exact behaviour, [GRAMMAR.md](docs/GRAMMAR.md)
  for syntax, and [CHANGELOG.md](CHANGELOG.md) under "Unreleased". Write what the
  code does, never what it is meant to do, and say plainly what it does not do.
- **No new dependency without a reason.** A new crate must be needed, maintained,
  under a licence compatible with ours, and pinned to a version at least two weeks
  old. Say why in the pull request.

## Code style

`cargo fmt` is not used: keep to the style of the file you are in. Comments explain
why, in full sentences. Error messages say what happened and what to do, in plain
words. Staal programs in tests and docs are formatted with `staalc fmt`.

## Pull requests

Keep a pull request to one change, with a description of what it does, why, how it
is tested, and which deliberate breaks the tests caught. Commits have a short first
line in the imperative ("Refuse a reply with no connection").

## Licence

Staal is licensed under the Apache License, Version 2.0, with the LLVM Exceptions
([LICENSE](LICENSE)). Under section 5 of that licence, anything you submit for
inclusion is licensed under the same terms, unless you say otherwise when you submit
it. You keep the copyright in your contribution. Only submit work you have the
right to submit.

"Staal" is a name, not part of the licence: section 6 of the licence grants no
right to use it. Forks and derived works need a name of their own.
