# Command-line reference

```text
staalc <command> [arguments]
staalc --help
staalc --version
```

`staalc` with no command prints the usage and exits 2; `--help` prints it and exits 0.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | The program has check errors, or the run failed. |
| 2 | A usage, file, config or journal problem. Nothing was run. |
| 3 | The run stopped and can be continued: resolve any action in doubt, then `staalc resume`. |

## Checking and tools

### `staalc check`

```text
staalc check <file.staal>
```

Parses the program and its modules and packages, and applies every compile-time rule.
Prints `<file>: ok`, or each error as `<file>:<line>:<column>: error[<code>]: <message>`
and the number of errors. See [Error codes](errors.md).

### `staalc fmt`

```text
staalc fmt [--check] <file.staal>...
```

Rewrites each file in the standard layout. With `--check`, rewrites nothing, lists the
files that are not formatted, and exits 1 if there are any.

### `staalc test`

```text
staalc test [--config <test.json>] [--engine interpreter|native|both] <file.staal or folder>...
```

Runs every function named `test_...` (no parameters, no result) in the given files,
and in every `*_test.staal` file under the given folders. Each test runs alone, as the
entry of its own run, without a journal. Without `--engine`, each test runs on both
engines when `staalc` was built with native code, and on the interpreter otherwise; it
passes only if every engine gives the same outcome and log.

A test holds no authority. `--config` may contain only `model`, `adapters` and
`limits`, and its model must be scripted (`"provider": "script"`). Without a config,
the model is scripted with no replies.

Prints one line per test, `ok` or `FAIL` with the failure's kind and message, then the
totals. Exits 0 when every test passes; 1 when a test fails, a file has check errors or
no tests, or a folder has no test files; 2 for a usage, file or config problem.

### `staalc doc`

```text
staalc doc <file.staal> [-o <out.md>]
```

Writes the program's documentation in Markdown to standard output, or to `<out.md>`:
the entry file, then each module it imports. A declaration's documentation is the
block of `//` lines directly above it. Only a program that passes every check is
documented; otherwise it exits 1 and writes nothing.

### `staalc repl`

```text
staalc repl [--engine interpreter|native|both]
```

Reads one input at a time: a declaration, a statement, or an expression, whose value
is printed as JSON. Each input is checked and run, with everything kept so far, as a
whole program. There are no capabilities, the model has no replies, and there is no
journal.

### `staalc lsp`

```text
staalc lsp
```

A Language Server Protocol server on standard input and output. It reports the errors
`staalc check` reports for the unsaved document, completes reserved words, built-ins
and the document's own names, and formats as `staalc fmt` does.

## Running

### `staalc run`

```text
staalc run <file.staal> --config <run.json> [--journal <run.journal>] [--log <run.jsonl>] [--engine native]
staalc run <file.staal> [--entry <function>] [--input <name>=<value>]... [--journal ...] [--log ...] [--engine native]
```

Runs a program. With `--config`, the run config gives the entry, inputs, grants,
secrets, model and limits ([Run config](run-config.md)). Without it, `--entry` names
the function (`main` by default) and each `--input` gives a parameter: a `Text`
parameter takes the value as written, any other parameter takes it as JSON. A function
with a capability or `Secret` parameter can only run with `--config`.

Prints the result as JSON on standard output, or `staalc: run failed: <kind>: <message>`
on standard error.

| Option | Effect |
| --- | --- |
| `--journal <file>` | Records every model call and action before and after it, so the run can be resumed after a crash. |
| `--log <file>` | Writes the run's log, one JSON event per line. |
| `--engine native` | Compiles eligible functions to machine code when the run starts. The result, log and journal are the same as on the interpreter. |

### `staalc resume`

```text
staalc resume <file.staal> (--config <run.json> | ...as run) --journal <run.journal> [--log <run.jsonl>] [--engine native]
```

Continues a run from its journal. Results already recorded are taken from the journal
instead of being performed again. The program, its config and the runtime must be the
ones that started the run; otherwise resume refuses.

### `staalc resolve`

```text
staalc resolve --journal <run.journal> (--performed <receipt> | --status <code> [--body <text>]
                                         | --reply <text> | --failed <reason> | --retry)
                                        [--operation <id>]
```

Records an operator's answer for an action whose outcome is unknown (a run that exited
3 with a failure of kind `in_doubt`). Check what happened in the real service first.

| Option | Records |
| --- | --- |
| `--performed <receipt>` | The action happened; the program continues with this receipt. |
| `--status <code> [--body <text>]` | For an HTTP request: the response the server gave. |
| `--reply <text>` | For a TCP or TLS exchange: the reply received. |
| `--failed <reason>` | The program sees a failed action. |
| `--retry` | The action did not happen; `resume` makes one attempt. |
| `--operation <id>` | Which action, when several are in doubt. |

Actions that are always safe to repeat (`fs.read`, `fs.write`, database queries,
`accept` and `reply` on sockets) are never in doubt, and `resolve` refuses them.

### `staalc config`

```text
staalc config <file.staal> [--entry <function>]
```

Prints a run config for the function: a placeholder for every input without a
default, a grant for every capability parameter with the limits its type declares,
every secret as an environment variable, and, if the function can reach a model call,
a Claude model whose key is read from `ANTHROPIC_API_KEY`.

### `staalc schema`

```text
staalc schema <file.staal> [--entry <function>]
```

Prints what a caller sees of the function, as JSON: `input`, a JSON Schema (2020-12)
of its inputs; `output`, a JSON Schema of its result; `grants` and `secrets`, the
parameters only an operator gives, with each grant's limits; and `reads_console`.

### `staalc profile`

```text
staalc profile <file.staal> (--config <run.json> | ...as run) [--engine native] [-o <profile.json>]
```

Runs the program as `run` does, without a journal, then prints on standard error, for
each function: calls, steps and time (in total, and its own without the functions it
called), and the model calls, tokens and actions it made. `-o` writes the same as
JSON. The output, result and exit code are those of `run`.

### `staalc debug`

```text
staalc debug <file.staal> (--config <run.json> | ...as run)
```

Runs the program on the interpreter, without a journal, stopping before the first
statement. Model calls and actions happen as in `run`. Commands: `break LINE`,
`break FILE:LINE`, `delete`, `breaks`, `continue` (`c`), `step` (`s`), `next` (`n`),
`finish`, `print NAME` (`p`), `locals`, `where` (`bt`), `list`, `quit` (`q`), `help`.
Secrets are shown by name only, capabilities as `"<capability>"`.

### `staalc build`

```text
staalc build <file.staal> -o <executable> [--runtime <libstaalc.a>]
```

Builds a standalone executable: the program's native functions compiled for the
processor family's baseline, linked with the runtime (`libstaalc.a`, found next to
`staalc` unless `--runtime` names it) using `cc`. Linux only, and only on a `staalc`
built with native code. The executable carries the program's source and refuses to run
if it was changed. Its commands:

```text
<executable> run     --config <run.json> [--journal <run.journal>] [--log <run.jsonl>]
<executable> resume  --config <run.json> --journal <run.journal> [--log <run.jsonl>]
<executable> resolve --journal <run.journal> (--performed <receipt> | --status <code> [--body <text>]
                                              | --failed <reason> | --retry)
```

Its journals are the same as `staalc`'s: a run started by one can be resumed by the
other.

## Packages

### `staalc fetch`

```text
staalc fetch [<project folder>]
```

Reads `staal.json`, chooses one version of each package that fits every range asking
for it, reads each from git into `.staal/packages/`, writes `staal.lock` with each
package's commit, version and the SHA-256 of every file, and prints each package's
authority. Needs `git`.

### `staalc update`

```text
staalc update [<project folder>] [--package <name>]
```

As `fetch`, but moves locked versions to the newest that fit their ranges; with
`--package`, only that package.

### `staalc publish`

```text
staalc publish --registry <registry folder> --git <location> --commit <id>
```

Adds a package version to a local clone of a registry, as a commit. Push the registry
to publish it.

### `staalc yank`

```text
staalc yank --registry <registry folder> --package <name> --version <version> [--undo]
```

Marks a published version not to be chosen again; projects that locked it keep it.
`--undo` reverses it.

## Serving and agents

### `staalc mcp`

```text
staalc mcp <manifest.json>
staalc mcp status <manifest.json>
```

Serves programs as tools to an MCP client on standard input and output, as the
manifest describes. `status` lists unfinished runs and each tool's quota use. See
[Serving tools over MCP](../guides/mcp.md).

### `staalc agent`

```text
staalc agent describe [<folder>]
staalc agent check [<folder>]
```

`describe` computes what each tool of the agent in the folder can do, and writes
`agent.described.json`. `check` checks `agent.json`, the agent's project, and that
`agent.described.json` is exactly what `describe` would write. See
[Publishing an agent](../guides/agents.md).
