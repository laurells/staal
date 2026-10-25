# Staal

[![ci](https://github.com/laurells/staal/actions/workflows/ci.yml/badge.svg)](https://github.com/laurells/staal/actions/workflows/ci.yml)

Staal is a programming language for software that acts on the real world:
payments, messages, files and calls to AI models. Its compiler proves, before a
program runs, that every action has the authority it needs, that untrusted model
output is validated before it is used, and that model spending stays inside a
declared budget. Its runtime records every effect durably, so a program that
crashes halfway through resumes without repeating a payment or a message.

Staal is said "stahl": it is Dutch and Afrikaans for steel. This repository holds
`staalc`, the Staal compiler and reference runtime, written in Rust.

## Highlights

**Safety checked at compile time.** Eight rules (E001 to E008) reject a program
that would act without a capability, widen a capability, use a model's output
without validating it, let untrusted text instruct a model, exceed a model
budget, call a model inside an unbounded loop, leak data marked as blinded to a
model, or claim completion without evidence.

**Authority you can read.** An action needs a capability in scope, and the
capability's limits (`only`, `uses`, `each`, `max`) are enforced by a broker
before the action runs. A package declares the authority it needs in its
manifest, and the compiler refuses code that reaches beyond it.

**Exact money, bounded everything.** Money is a decimal with a currency, never
floating point. Overflow, division by zero, mixed currencies and out-of-range
indexes are failures, not wrong answers. Every list declares a maximum length.

**Crash-safe by construction.** Every external effect is written to a journal
before it happens and after it returns. A crashed run resumes where it stopped;
an action whose outcome is unknown is stopped for an operator to resolve, never
retried blindly.

**Native code and standalone executables.** Pure functions compile to machine
code through Cranelift, with results, errors and journals identical to the
interpreter. `staalc build` links a program into a single executable.

**Callable from any language.** A C interface (`include/staal.h`) runs programs
from other languages; a Python wrapper is included.

## A first program

```
type Line { item: Text, price: Money, qty: Int }

fn line_total(line: Line) -> Money {
    return line.price * line.qty
}

fn total(lines: List<Line, 20>) -> Money {
    let sum = 0.00 NGN
    for line in lines {
        set sum = sum + line_total(line)
    }
    return sum
}
```

```
$ staalc run examples/invoice/invoice.staal --config examples/invoice/run.json
{"amount":"187501.50","currency":"NGN"}
```

The [language guide](docs/GUIDE.md) covers capabilities, models, budgets,
evidence, modules, packages and the rest.

## Getting started

Requirements: Rust 1.89 or newer ([rustup.rs](https://rustup.rs)), `git` to
fetch packages, and a C compiler (`cc`) to build executables. Developed and
tested with Rust 1.97.0.

```
git clone https://github.com/laurells/staal.git
cd staal
cargo build --release
./target/release/staalc check examples/invoice/invoice.staal
./target/release/staalc run examples/invoice/invoice.staal --config examples/invoice/run.json
```

To build without the native compiler (smaller, interpreter only):
`cargo build --release --no-default-features`.

## Command line

| Command | What it does |
| --- | --- |
| `staalc check <file.staal>` | Checks a program and reports errors by code |
| `staalc run <file.staal> --config <run.json>` | Runs a program; add `--journal` for crash-safe runs and `--engine native` for native code |
| `staalc resume <file.staal> --config <run.json> --journal <run.journal>` | Continues a run that stopped or crashed |
| `staalc resolve --journal <run.journal> ...` | Records an operator's decision on an action left in doubt |
| `staalc fetch [<project folder>]` | Fetches the packages named in `staal.json` and writes `staal.lock` |
| `staalc build <file.staal> -o <app>` | Builds a standalone executable (Linux) |
| `staalc fmt [--check] <file.staal>...` | Formats files in place, or lists those not formatted |
| `staalc test [--config <test.json>] <file or folder>...` | Runs the tests written in Staal, in both engines |
| `staalc doc <file.staal> [-o <out.md>]` | Writes the program's documentation in Markdown |
| `staalc repl` | Runs Staal one input at a time |
| `staalc profile <file.staal> --config <run.json>` | Runs a program and shows what each function spent |
| `staalc debug <file.staal> --config <run.json>` | Runs a program, stopping at breakpoints and steps |
| `staalc lsp` | A language server for editors: the same errors as `check`, completion, formatting |

Exit codes: `0` success; `1` check errors or a failed run; `2` a usage, file,
config or journal problem (nothing was run); `3` the run stopped and can be
continued.

## Documentation

| Document | Contents |
| --- | --- |
| [docs/GUIDE.md](docs/GUIDE.md) | The language: rules, syntax, capabilities, packages, native code |
| [docs/RUNTIME.md](docs/RUNTIME.md) | The runtime specification: semantics, the journal, failure behaviour |
| [editors/vscode](editors/vscode) | The VS Code extension: highlighting and the language server |
| [docs/GRAMMAR.md](docs/GRAMMAR.md) | The complete syntax, checked against the parser by the tests |
| [examples/README.md](examples/README.md) | Programs that call Claude and send real SMS and email, and how to run them with your keys |
| [packages/README.md](packages/README.md) | The reference packages and how a service becomes a library |
| [include/staal.h](include/staal.h) | The C interface |
| [CHANGELOG.md](CHANGELOG.md) | What changed in each version |

## Status

Staal is pre-release and not yet ready for production use. Built and tested:
the compiler and all eight safety rules, the reference runtime, Claude as a
model provider, exact money, the capability broker, budgets, the crash-safe
journal, modules, HTTP and file capabilities, capabilities declared by
libraries, packages fetched from git and locked by hash, native code,
executables and the C interface; enums and `match`, optional values, tuples, maps
and sets, typed errors, methods, generic functions, interfaces, function values and
closures; and a standard library of text, number, list, hashing, encoding and date
functions, with printing, the clock and random numbers journaled so a resumed run
replays them.

Not yet done: runs against the live services (Claude, Termii, Stripe, Resend,
Slack) with real keys, which so far are tested against local stand-ins; other
model providers; Windows; executables on macOS (`staalc build` is Linux only
for now); an installable Python package; and calling C libraries from Staal.

## Repository layout

```
src/lexer.rs, parser.rs, ast.rs   source text to syntax tree
src/checker.rs, budget.rs         type and safety rules, worst-case model cost
src/modules.rs, packages.rs       modules, manifests, staal.lock, fetching from git
src/pattern.rs, targets.rs        `only` patterns and canonical targets
src/interp/                       reference runtime: interpreter, money, broker,
                                  journal, HTTP, files, model providers (Claude)
src/native.rs                     native code through Cranelift
src/capi.rs, include/staal.h      the C interface
src/cli.rs, main.rs               the command line and built executables' entry
python/staal/                     Python wrapper over the C interface
docs/                             language guide and runtime specification
examples/                         example programs and libraries
packages/                         reference packages: sms, payments, email, chat
tests/                            fixtures, runtime cases, crash and engine tests
```

## Development

```
cargo test --features failpoints     # every test, with crash injection
cargo test --no-default-features     # without native code
cargo clippy --all-targets --features failpoints -- -D warnings
scripts/audit.sh                     # dependency advisories (needs network)
```

`tests/fixtures/pass/` holds programs that must compile and
`tests/fixtures/fail/` programs that must be rejected, each starting with
`// expect: CODE`. To add a rule, write the failing program first, then make the
checker reject it. CI runs every test on Linux, Apple Silicon and Intel macOS.

## Related repositories

The reference packages are also published on their own, so projects can depend
on them by git URL and commit:
[staal-sms](https://github.com/laurells/staal-sms),
[staal-payments](https://github.com/laurells/staal-payments),
[staal-email](https://github.com/laurells/staal-email) and
[staal-chat](https://github.com/laurells/staal-chat).

## License

Private and unlicensed: no permission is granted to use, copy or distribute this
code. A license will be chosen before any public release.
