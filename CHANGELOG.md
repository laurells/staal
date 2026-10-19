# Changelog

All notable changes to `staalc` are recorded here. Versions follow
[Semantic Versioning](https://semver.org); before 1.0, a minor version may change
the language or its file formats.

## Unreleased

### Added
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
