# Serving tools over MCP

`staalc mcp <manifest.json>` serves Staal programs as tools to Model Context Protocol
clients over standard input and output, the way an MCP client starts a local server.
Each tool call is an ordinary Staal run, with the same checks, grants, broker and
journal as `staalc run`. The client supplies only a call's arguments: grants, limits,
secrets, the model and deadlines come from the manifest, which the operator writes.

The full specification, including the HTTP transport, is [MCP.md](../MCP.md).

## A manifest

```staal file=refund.staal
// Refunds part of an order, never more than the grant allows.
fn main(order: Text, amount: Money, pay: cap payments.refund(each: 100 USD, uses: 1)) -> Text {
    let receipt = pay.refund(order, amount)
    return "Refunded " + to_text(amount) + " for order " + order
}
```

```json file=manifest.json
{
  "server": { "name": "shop-actions", "journals": "runs" },
  "tools": [
    {
      "name": "refund_order",
      "description": "Refund part of an order to the customer, within the shop's limits.",
      "program": "refund.staal",
      "grants": { "pay": { "cap": "payments.refund", "each": { "amount": "50.00", "currency": "USD" }, "uses": 1 } },
      "once": { "by": ["order"], "keep": "30d" },
      "quota": { "window": "24h", "calls": 100, "money": { "amount": "500.00", "currency": "USD" } }
    }
  ]
}
```

At start, the server checks every program and every tool's config as `staalc run`
would, without running anything, and refuses to start if any is wrong. Paths are
relative to the manifest. The manifest holds environment variable names, never secret
values.

```console
$ staalc mcp status manifest.json
no run is unfinished
tool refund_order: 0 of 100 calls, 0.00 USD of 500.00 USD in the last 1d
```

`staalc mcp status` lists unfinished runs and each tool's quota use. It reads the
journal directory without locking it, so it can run beside the server.

## Manifest keys

| Key | Contents |
| --- | --- |
| `server.name` | The name reported to clients. |
| `server.journals` | The directory of journals and the ledger. Created with mode 0700; if it exists, it must be owned by the server's user and not writable by group or others. |
| `server.keep` | Days a finished run's journal and log are kept, at least 1, or `"forever"`. Default 90. |
| `server.max_concurrent_runs` | Runs in progress at once across all tools, 1 to 256. Default 8. |
| `server.notify`, `server.notify_env` | A command run when a run stops for an operator, and the environment variables it may see. |
| `model` | The model for every tool, as in a run config. |
| `tools[].name` | 1 to 128 characters of `A-Z a-z 0-9 _ - .`, unique. |
| `tools[].title` | Optional display name. |
| `tools[].description` | Required: a model chooses tools by it. |
| `tools[].program`, `tools[].entry` | The program, and its entry function (default `main`). |
| `tools[].grants`, `secrets`, `model`, `net`, `fs`, `adapters`, `limits` | As in a [run config](../reference/run-config.md). `entry` and `inputs` are not allowed: inputs come from each call. |
| `tools[].deadline` | How long a run may keep starting actions, such as `"120s"`. |
| `tools[].max_concurrent_runs` | Runs of this tool in progress at once. |
| `tools[].once` | Arguments that identify one logical operation. |
| `tools[].quota` | Limits across calls. |
| `tools[].engine` | `"interpreter"` (default) or `"native"`. |

A program that can reach `read_line()` is refused, since standard input is the MCP
stream. A grant of `net.tcp.accept` or `net.tls.accept` needs `wait_ms`. Printed lines go
to the server's standard error.

## Connecting a client

A client that starts local servers takes a command and its arguments. For example, in
Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "shop-actions": {
      "command": "staalc",
      "args": ["mcp", "/path/to/manifest.json"],
      "env": { "ANTHROPIC_API_KEY": "..." }
    }
  }
}
```

The server lists each tool with the JSON Schema of its arguments, as `staalc schema`
prints it, and adds `staal_run_status`, a read-only tool that reports a run's status by
its id.

A call that succeeds returns the run's result. A refused or failed call returns
`isError: true` with the failure's message, such as `'payments.refund' allows at most
50.00 USD per action; this action needs 80.00 USD`. The server logs each finished run
to standard error.

## `once`: one logical operation

Without `once`, every call is a new run, and a call the client sends twice can act
twice. With `"once": { "by": ["order"], "keep": "30d" }`, calls with the same `order`
are one operation:

| The earlier run | A repeated call gets |
| --- | --- |
| still running | an error saying it is in progress, with its run id |
| done | the earlier result |
| failed | the earlier failure |
| waiting for an operator | the same stopped result |

A repeated key with different arguments is refused. A key is kept for `keep` after the
run ends (at most `server.keep`). The server logs a warning at start for each tool that
can spend money and has no `once`.

## Quotas

Each call is a new run with fresh limits, so `uses: 1` alone allows one action per call.
A quota bounds a tool across calls over a sliding window:

```json
{ "window": "24h", "calls": 100, "money": { "amount": "500.00", "currency": "USD" } }
```

`window` is `1m` to `30d`. Before a run starts, the server reserves one call and the
run's worst-case spend: the sum of each money-limited grant's `max`, or `each` times
`uses`. A call that would exceed the quota is refused and does not run. When the run
ends, the reservation is settled to what it actually spent. `money` requires every
money-limited grant to have `max`, or both `each` and `uses`, in the quota's currency.

## When a run waits for an operator

A run whose action may or may not have happened stops in doubt. The caller gets a
stopped result with the run id, which says the action may have happened and the tool
must not be called again for it. Resolution never happens over MCP: the caller cannot
know what the service did.

The server writes a record to `<journals>/waiting/<run id>.json`, logs it to standard
error, and runs `server.notify` if the manifest names it. `staalc mcp status` lists the
run with the `staalc resolve --journal ...` command to settle it. After `staalc resolve`,
the next start of the server resumes the run.

Before upgrading `staalc` or changing a program, run `staalc mcp status` and let every
unfinished run end: a changed program refuses to resume its unfinished runs, and the
server does not start while such a run exists.

## In an app

The same tool host runs inside an application through `staal.Tool` (Python) and
`staal.Tool.open` (Node.js), with the same keys. See [Calling Staal from an
app](apps.md).
