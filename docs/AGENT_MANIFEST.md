# The Staal agent manifest (draft for review)

**Status: a design, not built.** Nothing here exists in `staalc` yet except what it
says it reuses. It is written to be reviewed before any of it is built, as
[MCP.md](MCP.md) section 12 was.

An agent manifest describes one deployable agent: what it does, what it can touch,
the most it can do, and what happens when it fails. It describes the agent well enough
for a business to decide whether to run it, and for a marketplace or registry to list
it, without reading the code. Every claim it makes about authority is checked against
the compiled program. A claim it cannot check is labelled as a claim.

## 1. Principles

**Computed, not authored, wherever the program can answer.** What an agent can touch
is already in its program: the capability types of its entry functions, the limits
written in those types, the authority its packages declare and the compiler enforces
(E108), and its worst-case model use (E004). The manifest never asks the developer to
restate these. `staalc` computes them, so they cannot drift from the code or be
written to look smaller than they are.

**Reuse what exists.** An agent is a Staal project (`staal.json`, `staal.lock`)
whose tools are described as the tool host already describes them
([MCP.md](MCP.md) section 2, section 11). Installing an agent produces an ordinary
tool manifest, run by the ordinary host. The marketplace is never in the execution
path.

**A business grants; the agent only asks.** The manifest states what the agent needs
and suggests limits. The business's own grants decide what it gets, and those grants
can only narrow what the program allows, never widen it.

**Evidence is separate from description.** Whether an agent was tested, verified or
reviewed is not something its publisher writes in the manifest (section 6).

## 2. The files

An agent is a git repository at a pinned commit, holding a Staal project and one more
file:

| File | Written by | Holds |
| --- | --- | --- |
| `staal.json`, `staal.lock` | the developer, `staalc fetch` | the project and its locked packages, as today |
| `agent.json` | the developer | identity, description, tools, suggested limits, declared data handling |
| `agent.described.json` | `staalc agent describe` | everything computed from the program (section 4), never edited by hand |

`agent.described.json` is committed so a reader can see it without building anything,
and `staalc agent check` refuses an agent whose committed description differs from a
fresh one, so it cannot go stale or be edited by hand.

## 3. `agent.json`

The example is the refund agent of `examples/finops`, written as a deployable agent.

```json
{
  "agent": "support-refunds",
  "version": "0.1.0",
  "title": "Support refunds",
  "summary": "Refunds part of a customer's own order through the bank, records it, and texts the customer.",
  "publisher": { "name": "TeamOzy Technologies", "security_contact": "https://github.com/OWNER/REPO/security" },
  "license": "Apache-2.0",
  "categories": ["customer-operations", "finance"],
  "tools": [
    {
      "name": "refund_order",
      "description": "Refund part of one of the signed-in customer's orders, within the business's limits.",
      "program": "refund.staal",
      "entry": "main",
      "app_inputs": ["customer", "database"],
      "grants": {
        "pay":  { "suggested": { "each": { "amount": "100.00", "currency": "USD" }, "uses": 1 },
                  "needs": { "post": { "reach": ["https://BANK/refunds"], "idempotency_key": true } } },
        "text": { "suggested": { "uses": 1 } },
        "db":   { "suggested": {} },
        "book": { "suggested": {} }
      },
      "once": { "by": ["order"], "keep": "30d" },
      "quota": { "suggested": { "window": "24h", "calls": 200, "money": { "amount": "1000.00", "currency": "USD" } } },
      "deadline": "120s"
    }
  ],
  "data": {
    "declared": true,
    "reads": ["the customer's orders and phone number from the business's database"],
    "writes": ["the refunded amount on the order"],
    "sends": ["a refund to the bank", "one SMS to the customer's own number"]
  }
}
```

Fields:

- `agent`: the name, lowercase letters, digits and `-`, unique within a registry.
  `version`: semantic version. A published version never changes; its listing names
  the git commit it was published from.
- `title`, `summary`, `categories`: for people. `categories` from a fixed list the
  registry keeps.
- `publisher.security_contact`: where vulnerabilities are reported. Required.
- `license`: an SPDX expression.
- `tools`: one entry per tool, with the tool host's keys: `name`, `description`,
  `program`, `entry`, `app_inputs`, `once`, `deadline`. Two keys change meaning:
  - `grants`: for each capability parameter of the entry function, the **suggested**
    limits (the business may tighten or loosen them within the program's own
    ceiling), and for each runtime capability it needs, the **reach** it needs: the
    `only` patterns it expects to be granted, with placeholders (`BANK`) the business
    fills. Every parameter must appear, and only parameters may.
  - `quota.suggested`: a suggested quota. The business sets the real one.
- `data`: what the agent reads, writes and sends, in words. `"declared": true` marks
  it as the publisher's statement: the runtime enforces where data can go, not what a
  sentence says about it. Required, so a business sees it before installing.

Unknown keys are refused at every level, as in every Staal file.

## 4. `agent.described.json`: what `staalc` computes

`staalc agent describe` checks the project and writes, for the agent and each tool:

- **Program identity**: the program's fingerprint (the hash the journal already binds,
  covering every module and every package's commit), and each package's name, commit,
  and declared `authority`.
- **Interface**: the tool's input and output JSON Schemas, as `staalc schema` gives
  them today.
- **Authority**: for each capability parameter, its capability path and the limits the
  program itself writes in the type (`each`, `max`, `uses`, `only`): the program's
  ceiling, which no grant can exceed. `staalc schema` gives the paths today but not
  the limits written in the types: it gains them.
- **Reach**: the union of what the agent can contact, from its packages' declared
  authority and from the runtime capabilities it needs.
- **Secrets**: the secrets its grants need, by role (`key` of `post`), never by value
  or variable name.
- **Model use**: the worst-case model calls and tokens per run, from the budget the
  compiler already checks (E004); none for a tool that calls no model.
- **Failure behaviour**: for each action, what a crash does: replayed, resent with an
  idempotency key, or stopped for an operator. Taken from the grants' rules the runtime
  already applies (RUNTIME.md section 11).
- **Unchecked parts**: whether any path reaches C code (`cap ffi`) or console input.
  An agent with C code is marked unchecked, as its runs are.

## 5. The checks

**At publish** (`staalc agent check`, and the registry runs it again on the pinned
commit; it refuses on any failure):

1. The project checks with no errors, its packages verify against `staal.lock`, and
   `agent.described.json` equals a fresh description.
2. Every capability parameter of every tool entry is in `grants`, and nothing else is.
3. Every `suggested` limit fits the program's ceiling (section 4), so a suggestion can
   never be larger than what the code allows.
4. Every `reach` pattern is canonical, has no `*` in a host, and is covered by what the
   capability can be granted.
5. The tool host would accept the tool: no console input, no accept without
   `wait_ms`, a valid `once`, quota and deadline (MCP.md section 2).
6. `security_contact`, `license` and `data` are present.

**At install** (a business's `staalc agent install`, which writes an ordinary tool
manifest from `agent.json` and the business's settings):

1. The agent is fetched by version and verified: the commit its listing names, and the
   files against their hashes, as packages are today.
2. The business sets every grant's limits and fills every reach placeholder. A grant
   wider than the agent's declared `reach` is refused unless the business marks it as
   intended (`"wider_than_declared": true`), which the generated manifest records.
3. A grant beyond the program's ceiling cannot be expressed: the runtime already
   refuses it.
4. Secrets are named by the business's environment variables; the manifest never holds
   a value.
5. The result is shown as one page before it is written: every capability, its limits
   and reach, the quotas, the worst-case spend per day, the model use, the failure
   behaviour and the data statement.

**At run time** nothing changes: the host and broker enforce the generated manifest as
they enforce any.

## 6. Trust designations

Three, never merged, and never written by the publisher:

| Designation | Means | Evidence the listing shows |
| --- | --- | --- |
| **Published** | passed the publish checks (section 5) | the checks' output and the pinned commit |
| **Verified** | also passed a defined set of technical checks run by the registry: the tests pass, the tool host starts it, the crash tests of its actions pass, its fuzz targets ran clean for a set time | each check, its date and the `staalc` version it ran with |
| **Independently reviewed** | also assessed by a reviewer outside the publisher | the reviewer, the date, the version reviewed, the report or its summary, and findings still open |

A designation belongs to one version and is not inherited by the next. Verification
expires when its checks change or the agent's dependencies change. A review is never
shown for a version it did not cover, and open findings are always shown beside it. An
agent with C code can be Published, never Verified.

## 7. Not in this version

Each would be its own change to this document:

- **Human approval before an action.** Staal has no approval step today; the only
  human in the loop is the operator who resolves a run stopped in doubt. An approval
  capability (an action that waits for a person, journaled like any other) would come
  first, then a manifest field for it.
- **Agent-to-agent delegation**, with authority that only narrows as it passes.
- **Signed listings.** Until then, a listing is as trustworthy as its git commit and
  the registry's review of changes, as with packages today.
- **Hosted deployment.** The business runs the host; a hosted service storing
  businesses' credentials is a separate product needing its own security review.
- **Agents not written in Staal.** Accepting them through adapters is possible later;
  they could never hold Verified or Independently reviewed, since their authority is
  not enforced by Staal.

## 8. Questions for the reviewer

1. Should `suggested` limits be allowed above the program's ceiling at all? This
   draft says no.
2. Should a grant wider than `reach` be allowed with `"wider_than_declared"`, or
   refused outright?
3. Is `data` worth requiring when it cannot be checked, or does it give false comfort?
4. Should the registry run Verified checks itself (cost, and responsibility when an
   agent misbehaves), or should publishers attach evidence the registry re-runs?
5. Who may grant Independently reviewed: any named reviewer, or reviewers the registry
   accepts?
