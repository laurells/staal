# Publishing an agent

An agent is a folder that a business can review and install: a Staal project whose
entry functions are tools, an `agent.json` written by the publisher, and an
`agent.described.json` that `staalc` computes from the code. The format is specified in
[AGENT_MANIFEST.md](../AGENT_MANIFEST.md), at format version `0.1`.

What exists today: `staalc agent describe` and `staalc agent check`. There is no agent
registry and no `staalc agent install` yet; a business installs an agent by writing a
tool manifest for `staalc mcp` or a `staal.Tool` from it.

[Your first agent](../get-started/first-agent.md) builds a complete agent with a model
and a payment capability. This page uses a minimal one.

## The folder

```staal file=notify.staal
type Message { text: Text }

// Posts a message to the team's chat webhook.
fn notify(text: Text, hook: cap net.http.post(uses: 1)) -> Int {
    let r = hook.post("https://hooks.example.com/team", json(Message { text: text }), [header("Content-Type", "application/json")])
    return r.status
}
```

```json file=staal.json
{}
```

```json file=agent.json
{
  "manifest": "0.1",
  "agent": "team-notify",
  "version": "0.1.0",
  "requires_staalc": ">=0.29.0",
  "title": "Team notify",
  "summary": "Posts one message to the team's chat webhook.",
  "publisher": { "id": "github:your-account", "name": "Your Name", "security_contact": "https://github.com/your-account/team-notify/security" },
  "license": "Apache-2.0",
  "categories": ["communications"],
  "tools": [
    {
      "name": "notify_team",
      "description": "Post one message to the team's chat.",
      "program": "notify.staal",
      "entry": "notify",
      "grants": {
        "hook": { "suggested": { "uses": 1 }, "reach": ["https://hooks.example.com/team"] }
      },
      "deadline": "30s"
    }
  ],
  "data": {
    "reads": ["the message the caller gives"],
    "writes": [],
    "sends": ["the message to the team's chat webhook"],
    "categories": ["messages"],
    "processors": ["the team's chat service"],
    "retention": "nothing is kept by the agent beyond the business's own journals",
    "model_training": "no model is called",
    "regions": "wherever the team's chat service processes data"
  }
}
```

```console
$ staalc agent check .
staalc: cannot read ./agent.described.json: No such file or directory (os error 2); run `staalc agent describe`
$ staalc agent describe .
agent team-notify 0.1.0: described in agent.described.json (1 tool(s))
$ staalc agent check .
agent team-notify 0.1.0: ok (1 tool(s))
```

## `agent.json`

Written by the publisher. Unknown keys are refused at every depth;
[docs/schemas/agent.schema.json](../schemas/agent.schema.json) is its JSON Schema.

| Key | Contents |
| --- | --- |
| `manifest` | `"0.1"`. |
| `agent` | Lowercase letters and digits in parts joined by single `-`, at most 64 characters. |
| `version` | A semantic version. |
| `requires_staalc` | A version range that includes the checking `staalc`. |
| `title`, `summary` | What the agent is. |
| `publisher` | `id` (`github:<account>`), `name`, and `security_contact`, where vulnerabilities are reported. |
| `license` | An SPDX expression. |
| `categories` | From: commerce, communications, customer-operations, data, education, finance, healthcare, human-resources, it-operations, legal, logistics, marketing, productivity, sales, security, software-development. |
| `tools` | One entry per tool (below). |
| `data` | The data handling declaration: `reads`, `writes`, `sends`, `categories`, `processors`, `retention`, `model_training`, `regions`. |

Each tool:

| Key | Contents |
| --- | --- |
| `name`, `description` | The tool's name and what a model reads to choose it. |
| `program`, `entry` | The program file and its entry function. |
| `grants` | An entry for every capability parameter of the entry function, and nothing else. |
| `grants.<name>.suggested` | Limits the publisher recommends (`each`, `max`, `uses`, `only`). They must fit within the limits the code declares. |
| `grants.<name>.reach` | For a runtime capability that takes targets (`net.http.*`, `net.tcp.exchange`, `net.tls.exchange`, `fs.*`, `db.sqlite.*`): the targets it is expected to reach. `{{NAME}}` stands for a host the business supplies at install. |
| `grants.<name>.needs` | For a library capability: the same, for each capability it needs. |
| `app_inputs` | Inputs the business's application supplies, never the caller. |
| `app_input_requirements` | For each app input, what the business must supply. Required for each. |
| `once`, `quota.suggested`, `deadline` | As for a tool in `staalc mcp`'s manifest. |

`suggested` and `reach` are statements, not authority: the business sets the real
grant, which can never exceed the limits in the code. The data handling declaration is
the publisher's statement; Staal cannot check it.

A capability served only by the runtime's simulated service, such as
`payments.refund`, is refused in an agent: an agent's capabilities reach real services,
through the runtime's own capabilities or a library capability built on them.

## `agent.described.json`

Computed by `staalc agent describe` from the checked program and its packages. For each
tool, it records:

| Field | Contents |
| --- | --- |
| `grants`, `consumers` | Every capability the tool can use, with the limits the code declares, and every holder of it: the agent's code, or a library capability's need. |
| `inputs` | Each input and its provenance: `caller` or `app`. |
| `interface` | JSON Schemas of the input and the output. |
| `model_use` | The worst-case model calls and tokens of one run, or `"unbounded"`. |
| `expenditure_ceiling` | The most money one run can spend, per currency: an amount, or `unbounded`. |
| `failure` | For each runtime capability: how an interrupted action is recovered (`repeat`, `resend_with_key` or `operator`), whether that relies on the service deduplicating, and whether its outcome can be unknown. |
| `network` | For each capability that leaves the machine: its declared `reach`, the `only` its type writes, what is enforced (canonical targets, no redirects, TLS), and the addresses it may connect to. |
| `unchecked` | Whether the tool can call C functions or read console input. |
| `package_boundaries` | For each package: its declared authority, and what the tool can hand it. |
| `fingerprint`, `program_sha256` | The fingerprint scheme (`staal-program-v3`), and the program's hash, which every run's journal binds. |

The description is deterministic: the same source gives the same bytes.

## `staalc agent check`

`check` refuses the folder unless:

1. `manifest` is a version this `staalc` knows, and `requires_staalc` includes it;
2. the project checks with no errors, its packages match `staal.lock`, and
   `agent.described.json` is exactly what `describe` writes now;
3. every tool's `grants` names exactly the entry's capability parameters;
4. every suggestion fits the code's limits, and every `reach` pattern is canonical and
   within them;
5. each tool is one the tool host accepts: no console input, no `accept` without
   `wait_ms`, a valid `once`, quota and deadline;
6. `publisher`, `security_contact`, `license` and `data` are present.

Run `staalc agent describe` after every change to the program or its packages, and
commit `agent.described.json` with it.
