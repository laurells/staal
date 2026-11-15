# Native code and executables

A run uses one of two engines. The interpreter is the default and the reference. The
native engine compiles the functions that qualify to machine code with Cranelift, in
memory, when the run starts; every other function runs on the interpreter. Both
engines give the same results, failures, logs and journals. The full rules are in
[RUNTIME.md](../RUNTIME.md), sections 15 and 17.

## Choosing the engine

The engine is chosen for each run, never in the run config:

| Where | How |
| --- | --- |
| `staalc run`, `resume`, `profile` | `--engine native` |
| `staalc test`, `repl` | `--engine interpreter`, `native` or `both` |
| Python | `lib.run(..., engine="native")` |
| Node.js | `staal.run(program, config, { engine: 'native' })` |
| C | the `engine` argument, `"interpreter"` or `"native"` |
| `staalc mcp` manifest, `staal.Tool` | `"engine": "native"` |

The journal does not record the engine: a run that stopped on one engine resumes on
the other.

`staalc` built with `--no-default-features` has no native engine; `--engine native`
and `staalc build` are then refused. The release archives, the Python package and the
Node.js package include it.

## Which functions qualify

A function qualifies when:

- its parameters and result are `Int`, `Bool`, `Text`, `Money`, or records, enums,
  optional values, tuples, lists, maps or sets of these, or function values of
  qualifying functions;
- its body uses only `let`, `set`, `if`, `match`, `repeat`, `loop`, `for`, `break`,
  `continue`, `return` with a value, `fail`, literals, arithmetic, comparisons, `and`,
  `or`, `not`, field reads, record and variant literals, `some`, `none`, tuples, map
  literals, indexing, calls to qualifying functions and function values, and the
  built-in functions except `header`, `fill_secret`, `print`, `now` and `random_int`;
- every function it calls qualifies.

Model calls, actions, `try`, `parse`, `validate`, network and file access, budgets,
printing, the clock, random numbers and the journal therefore never run in native
code. Native code is entered only when the interpreter calls a compiled function.

```staal file=order.staal
type Item { name: Text, price: Money, quantity: Int }

fn total(items: List<Item, 100>) -> Money {
    let sum = 0 USD
    for item in items {
        set sum = sum + item.price * item.quantity
    }
    return sum
}

fn main(items: List<Item, 100>) -> Text {
    print("items: " + json(len(items)))
    return to_text(total(items))
}
```

`total` qualifies; `main` calls `print`, so it runs on the interpreter and calls the
compiled `total`.

## What stays the same

Native code:

- counts steps against `max_steps` and checks `max_depth` exactly as the interpreter
  does;
- checks every Int `+`, `-`, `*`, `/` and unary `-`, and fails on overflow, division by
  zero and `MIN / -1` with the interpreter's message;
- checks Money operations for two currencies, overflow and results below zero, and
  reports failures through the interpreter's own functions;
- evaluates left to right; `and` and `or` evaluate their right side only when needed;
- performs every operation on Text, records, lists, maps and sets with the
  interpreter's functions, in an arena the runtime owns. Int, Bool and Money are held
  in machine words inside a function.

The test suite runs every runtime case on both engines and compares results, logs and
journals. `staalc test` runs each test on both engines by default and fails if they
differ.

## Standalone executables

`staalc build` compiles a program into one executable. Linux only.

```sh
staalc build order.staal -o order
```

```text
order: built, 1 function(s) as native code
```

1. The program is checked as by `staalc check`, including packages and authority. A
   program with errors is not built (exit code 1).
2. Every qualifying function is compiled for the baseline of the processor family, so
   the executable runs on any machine of that family.
3. The executable carries the program's source files, the SHA-256 of its fingerprint,
   and the names of the compiled functions.
4. It is linked with `libstaalc.a` using `cc`. `libstaalc.a` is found next to `staalc`
   (it is in the release archive), or named with `--runtime`; after `cargo install`,
   pass `--runtime target/release/libstaalc.a` from the source folder.

The executable takes `run`, `resume` and `resolve` with the same options and exit codes
as `staalc`, without the program file and without `--engine`:

```text
order run     --config <run.json> [--journal <run.journal>] [--log <run.jsonl>]
order resume  --config <run.json> --journal <run.journal> [--log <run.jsonl>]
order resolve --journal <run.journal> (--performed <receipt> | --status <code> [--body <text>]
                                       | --failed <reason> | --retry)
```

At start it checks its program again from the sources it carries, and refuses to run
(exit code 2) unless the fingerprint and the compiled functions match. Package
authority is not checked again. Its journals are bound to the program's fingerprint: a
run started by the executable can be resumed by `staalc resume` with the same program,
and the reverse.
