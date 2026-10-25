# Changelog

All notable changes to `staalc` are recorded here. Versions follow
[Semantic Versioning](https://semver.org); before 1.0, a minor version may change
the language or its file formats.

## Unreleased

### Added
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
