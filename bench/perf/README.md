# Speed: Staal beside Rust, Go, Python and TypeScript

The same nine workloads, written once in each language, timed as whole processes.
`python3 run.py` builds every program, checks that every one gives the same answer,
then times each five times. Results are in [results/RESULTS.md](results/RESULTS.md),
with the raw numbers in `results/results.json`. CI does not run it.

Staal is timed three ways. **Interpreter**: `staalc run --engine interpreter`.
**Native**: `staalc run --engine native`, which compiles qualifying functions with
Cranelift when the run starts (RUNTIME.md section 15). **Executable**: a program built
by `staalc build`, run with `<exe> run --config`. All three check the program when
they start, and count each step against `max_steps`, which is raised here so no
workload reaches it.

## The workloads

| Workload | What it does | Exercises |
| --- | --- | --- |
| startup | returns 1 | starting the runtime, and in Staal checking the program |
| fib | recursive Fibonacci of n | function calls, integer arithmetic |
| loop | sums `(i * i) % 7` for i below n | a tight loop, the remainder |
| money | totals n invoice lines, each 0.37 USD times 1 to 10 | exact money arithmetic |
| lists | appends n pseudo-random numbers to a list one by one, then sorts it | building a collection |
| maps | counts n keys, from 1000 distinct ones, in a map | a map updated many times |
| bigmap | puts n distinct keys in a map in scattered order, then looks each up | a large map |
| text | joins the numbers below n with commas, and measures the text | building a long text |
| textread | measures a text of n characters 1000 times, and searches it for a number | reading a long text |

## How each language is written

Each program is written the way that language is usually written, so the comparison
is of ordinary code, not of each language at its most tuned.

- **Rust**: `rustc -C opt-level=3 -C codegen-units=1`, with **overflow checks on**,
  because Staal checks every integer operation and stops on overflow; Rust's release
  default leaves them off. Money as integer cents (`i64`).
- **Go**: `go build`. Integers wrap on overflow, unchecked. Money as integer cents.
  Text through `strings.Builder`.
- **Python**: CPython, the work inside a function. Money as `decimal.Decimal`, which is
  what Python code uses for exact money. Text as a list of parts joined once.
- **TypeScript**: compiled by `tsc` and run on Node.js; and the same source run by
  Bun. Money as integer cents in a `number`, exact below 2^53. The pseudo-random step
  uses `Math.imul`, because the product exceeds what a `number` holds exactly. A
  text's length in characters is `[...t].length`: `t.length` counts UTF-16 units.
- **Staal**: as the language has it. A list grows by `append`, a map by `put` and a
  text by `+`, each written `set x = f(x, ...)`; values do not change, but one that
  nothing else holds is added to in place (RUNTIME.md section 15).

Every language computes the same answer for every size; `run.py` stops if any
disagrees.

## What these numbers do and do not say

They are whole-process times on one machine, a shared cloud virtual machine with 2
cores, so small differences (a few milliseconds, or ten percent) are noise; the
fastest and slowest of each five runs are in `results.json`. Startup is in every
number, which matters most for the short ones.

They measure computation only. Staal is a language for agents: a run's time is
usually spent waiting for a model or a service, and those calls, its journal and its
limits are what it is for. None of that is measured here.

## What they show about Staal

Read with RESULTS.md. Since the first measurement, the runtime has changed in ways
the CHANGELOG lists: Int built-ins and money computed in machine code, values shared
and added to in place, maps and sets as trees, and the interpreter's variables held
in one list per call.

- **Native code is within a few times Rust and Go on computation**: `fib` at twice
  Rust's time, `loop` at three times, money at four (8.8 ms, where it took 211 ms).
  It is faster than Node.js on all of them, about level with Bun on `fib` and money,
  and twice Bun's speed on `loop`. Every Int and
  money operation is still checked, and every step counted, which Rust (with
  overflow checks) does only in part and Go not at all.
- **Collections and text are built in linear time**, at 4 to 11 times Rust's time:
  about Python's speed (twice it on lists, a third slower on text in native code).
  Each operation still goes through the runtime's general functions for values.
- **A large map is the weakest**: 100,000 distinct keys in 108 ms, ten times Rust,
  under three times Python and Node.js and nearly five times Bun, which use hash
  tables. A Staal
  map is a tree, so its keys are always in order, which a hash table's are not. It
  took 13.8 s before maps were trees.
- **Reading a long text is fast**: within 1.4 times Rust, ahead of Go and Python.
  Reading a variable shares the text rather than copying it.
- **The interpreter is slow**: 6 times CPython on `fib` and 4 times on `loop` (it was
  13 times). Each expression is a step through a tree of the program. Native code is
  what to use where computation matters; the interpreter is the reference both
  engines are held to.
- **Memory is small**: 10 to 28 MiB a run, against 42 to 86 MiB for Node.js.
