# Speed: Staal beside Rust, Go, Python and TypeScript

The same seven workloads, written once in each language, timed as whole processes.
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
| text | joins the numbers below n with commas, and measures the text | building a long text |

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
  uses `Math.imul`, because the product exceeds what a `number` holds exactly.
- **Staal**: as the language has it. A list grows by `append`, a map by `put` and a
  text by `+`; each makes a new value (values do not change), so building a
  collection of n items one at a time copies it n times.

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

Read with RESULTS.md, measured after two changes to the runtime (CHANGELOG): native
code computing `rem`, `abs`, `min` and `max` in machine code, and values held shared
and moved into a `set` that reads them once, so collections and texts are built in
place.

- **Native code is within a few times Rust and Go on calls, arithmetic and loops**:
  `fib` at about twice Rust's time, `loop` at about three times. Both are faster than
  Node.js, and level with Bun or ahead of it. Every Int operation is still checked and
  every step counted, which Rust (with overflow checks) does only in part and Go not
  at all.
- **Collections and text are built in linear time**, as in the other languages: a
  list of 10,000 in 15 ms (2.1 s before the change), a text of 50,000 pieces in 22 ms.
  They stay several times slower than Rust and Go, and are about Python's speed: each
  operation still goes through the runtime's general functions for values.
- **Money is the slowest**: about Python's `Decimal`, a hundred times Rust's integer
  cents. Money arithmetic in native code goes through the interpreter's functions for
  values, each with its currency checked; computing it in machine code, as Ints now
  are, is the next change to make.
- **The interpreter is slow**: about 13 times CPython on `fib`. Its variables are
  looked up by name in a map on every read. Native code is what to use where
  computation matters; the interpreter is the reference both engines are held to.
- **Memory is small**: 10 to 18 MiB a run, against 42 to 54 MiB for Node.js.
