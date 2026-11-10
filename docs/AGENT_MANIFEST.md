# Staal Agent Manifest Specification v0.1 (draft for review)

**Status: a design, not built.** Nothing here exists in `staalc` yet except what it
says it reuses. It is written to be reviewed before any of it is built, as
[MCP.md](MCP.md) section 12 was. v0.1 takes in the first review's decisions
(section 11).

An agent manifest describes one deployable agent: what it does, what it can touch,
the most it can do, and what happens when it fails. A business uses it to decide
whether to run the agent, and a registry or marketplace uses it to list the agent,
without either having to read the code.

## 1. Three sources of truth, never merged

| Source | Says | Written by | Trust |
| --- | --- | --- | --- |
| **Description** (`agent.json`) | what the publisher claims the agent does | the publisher | a claim |
| **Computed description** (`agent.described.json`) | what the program is capable of doing | `staalc`, from the compiled program | as strong as the compiler and runtime |
| **Execution record** (journals and logs of the business's own host) | what actually happened | the runtime, while it runs | a record |

Every screen that shows an agent says which source each statement comes from. No
statement moves from one source to another: a description cannot claim what only the
computed description can establish, and neither speaks for what happened in a run.

Three values of authority are kept apart in the same way:

    business grant  ⊆  program ceiling

The **program ceiling** is what the code permits: the limits written in its capability
types and its packages' declared authority, enforced by the compiler and runtime. The
**suggestion** is what the publisher recommends; it is guidance, never authority, and
must itself fit the ceiling. The **business grant** is what the business authorises:
it may be smaller than the suggestion, and can never exceed the ceiling, because the
runtime already refuses a grant that would.

## 2. Principles

**Computed wherever the program can answer.** What an agent can touch is already in
its program: the capability types of its entry functions, the limits written in them,
its packages' authority (E108), its worst-case model use (E004). `staalc` computes
these; the publisher never restates them.

**Reuse what exists.** An agent is a Staal project (`staal.json`, `staal.lock`). Its
tools are described as the tool host already describes tools ([MCP.md](MCP.md)
sections 2 and 11). Installing an agent produces an ordinary tool manifest, run by the
ordinary host. An agent is not a privileged object in the runtime: it is a packaged
Staal project with a business identity. The registry is never in the execution path,
and a deployed agent runs whether or not the registry is reachable.

**Fail closed.** Unknown keys, unknown manifest versions, and anything that cannot be
checked are refused, as in every Staal file.

## 3. The files

An agent is a git repository at a pinned commit, holding a Staal project and two more
files:

| File | Written by | Holds |
| --- | --- | --- |
| `staal.json`, `staal.lock` | the developer, `staalc fetch` | the project and its locked packages, as today |
| `agent.json` | the developer | identity, description, tools, suggestions, data handling declaration |
| `agent.described.json` | `staalc agent describe` | everything computed from the program (section 5) |

`agent.described.json` is committed so it can be read without building anything.
`staalc agent check` refuses an agent whose committed description differs from a
fresh one, so it cannot go stale or be edited by hand.

## 4. `agent.json`

The example is the refund agent of `examples/finops`, written as a deployable agent.

```json
{
  "manifest": "0.1",
  "agent": "support-refunds",
  "version": "0.1.0",
  "requires_staalc": ">=0.28.0",
  "title": "Support refunds",
  "summary": "Refunds part of a customer's own order through the bank, records it, and texts the customer.",
  "publisher": { "id": "github:OWNER", "name": "Example Publisher", "security_contact": "https://github.com/OWNER/REPO/security" },
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
    "reads": ["the customer's orders and phone number from the business's database"],
    "writes": ["the refunded amount on the order"],
    "sends": ["a refund request to the bank", "one SMS to the customer's own number"],
    "categories": ["contact details", "payment references"],
    "processors": ["the business's bank", "the business's SMS provider"],
    "retention": "nothing is kept by the agent beyond the business's own journals",
    "model_training": "no model is called",
    "regions": "wherever the business's bank and SMS provider process data"
  }
}
```

- `manifest`: the specification version, `"0.1"`. A tool that does not know the
  version refuses the file (section 9).
- `agent`: lowercase letters, digits and `-`, unique in a registry. `version`: a
  semantic version; a published version never changes, and its listing names the commit
  it came from. `requires_staalc`: the oldest `staalc` that can check and run it.
- `publisher.id`: the account that publishes, verified by the registry (section 8).
  `name` is shown beside it and never alone. `security_contact`: where vulnerabilities
  are reported; required.
- `license`: an SPDX expression. `categories`: from a fixed list the registry keeps.
- `tools`: one entry per tool, with the tool host's keys `name`, `description`,
  `program`, `entry`, `app_inputs`, `once` and `deadline`, and two that change meaning:
  - `grants`: for every capability parameter of the entry function, the `suggested`
    limits, and for each runtime capability it needs, the `reach` it expects: the
    `only` patterns, with placeholders (`BANK`) the business fills at install.
    Every parameter must appear, and only parameters may.
  - `quota.suggested`: a suggested quota; the business sets the real one.
- `data`: the **data handling declaration**, required. Every surface shows it under
  the heading "Data handling declaration: publisher supplied, not compiler verified".
  `reads`, `writes` and `sends` describe intended data flow; `categories`,
  `processors`, `retention`, `model_training` and `regions` are operational and
  contractual statements Staal cannot check. A business holds the publisher to them;
  Staal does not claim to.

## 5. `agent.described.json`: what `staalc` computes

For the agent, and for each tool:

- **Program identity**: the program's fingerprint (the hash the journal binds,
  covering every module and every package's commit), the `staalc` version that
  computed it, and each package's name, commit and declared authority.
- **Interface**: input and output JSON Schemas, as `staalc schema` gives them.
- **Authority**, in two layers that every surface shows apart:
  - *Declared package authority*: what each package may contact (its `staal.json`),
    which the compiler holds it to.
  - *Program ceiling*: each capability parameter's path and the limits the program
    writes in its type (`each`, `max`, `uses`, `only`). `staalc schema` reports the
    paths today but not these limits: it gains them.
  A third layer, *effective authority*, exists only after install (section 6), and a
  fourth, *authority exercised*, only in a run's execution record.
- **Model use**: the worst-case model calls and tokens per run, from the budget the
  compiler checks (E004). Counts, never money: a price is a registry's display, dated
  and naming the price list it used (section 7).
- **External expenditure ceiling**: for each money-moving capability, the most one run
  can move under the program's own ceiling, by currency. Shown apart from model use: a
  tool with no model calls can still move money.
- **Failure behaviour**, per action, in three separate parts:
  - *Runtime recovery*: what Staal does after a crash: replays a completed action,
    resends an action in doubt with the same idempotency key, repeats an action that
    is safe to repeat, or stops the run for an operator. This is Staal's guarantee.
  - *External idempotency*: whether the grant relies on the service to deduplicate
    (`idempotency_key`). This is the **service's** guarantee, not Staal's, and is
    shown as such: Staal sends the same key; the service must honour it.
  - *Outcome certainty*: whether the action's result can be unknown after a crash,
    and if so that the run stops for an operator rather than guessing.
- **Unchecked parts**: whether any path reaches C code (`cap ffi`) or console input.
  C code makes the agent unchecked, as its runs are.

## 6. The checks

**At publish** (`staalc agent check`, and the registry runs it again on the pinned
commit; any failure refuses):

1. `manifest` is a version this `staalc` knows; `requires_staalc` is satisfied.
2. The project checks with no errors, its packages verify against `staal.lock`, and
   `agent.described.json` equals a fresh description.
3. Every capability parameter of every tool entry is in `grants`, and nothing else is.
4. Every suggestion fits the program ceiling.
5. Every `reach` pattern is canonical, and its `*`, if any, comes after
   `scheme://host/`, as grants already require (RUNTIME.md, Targets), so a reach can
   never name another server by pattern.
6. The tool host would accept each tool: no console input, no accept without
   `wait_ms`, a valid `once`, quota and deadline (MCP.md section 2).
7. `publisher`, `security_contact`, `license` and `data` are present.

**At install** (`staalc agent install`, run by the business on its own machine; it
writes an ordinary tool manifest from `agent.json` and the business's settings):

1. The agent is fetched by version and verified: the commit its listing names, and
   every file against its hash, as packages are today.
2. The business sets every grant's limits and fills every reach placeholder.
3. **A grant wider than the declared reach is refused.** It can be granted only with an
   explicit override naming each wider pattern and a written reason
   (`"wider_than_declared": [{"pattern": "...", "reason": "..."}]`). The override is
   recorded with its reason in the generated manifest; every run's journal binds the
   run config built from it, which holds the wider grant, so the execution record shows
   what was granted. No override reaches past the program ceiling: the
   runtime refuses that whatever the manifest says.
4. Secrets are named by the business's environment variables; nothing holds a value.
5. One page is shown before anything is written: for every capability, the declared
   package authority, the program ceiling, the effective grant and any override, side
   by side; the quotas; the model use; the external expenditure ceiling per run and per
   quota window; the failure behaviour in its three parts; and the data handling
   declaration under its heading.

**At run time** nothing changes: the host and broker enforce the generated manifest
as they enforce any, and the journal records the authority exercised.

## 7. Trust designations

Three, never merged, never written by the publisher, each for one version only:

| Designation | Means | Evidence shown |
| --- | --- | --- |
| **Published** | the publish checks passed, re-run by the registry | the checks' output, the commit, the `staalc` version |
| **Verified** | also passed checks the registry ran itself | below |
| **Independently reviewed** | also assessed by a registry-recognised reviewer outside the publisher | below |

**Verified** is run by the registry, never taken from the publisher. On the pinned
commit it fetches the source, verifies locks and hashes, runs the publish checks, the
agent's tests, the crash tests of its actions and its fuzz targets for a set time, and
records the exact `staalc`, runtime, dependency and test versions. It publishes that
evidence with the version. Publisher evidence may be shown beside it, labelled, and
never replaces it. The checks run in isolated, disposable environments with no network
beyond what the tests are given, resource limits, and no credentials but disposable
ones: running an untrusted agent's tests is itself an attack surface. A clean fuzz run
is shown as what it is: the named targets ran for the stated time without a finding.
An agent with C code can be Published, never Verified.

**Independently reviewed** is granted for a report from a reviewer the registry
recognises by published criteria: verified identity, relevant experience, areas of
expertise, a conflict-of-interest disclosure, a stated method, and a record of
assessments. The listing shows the reviewer, the date, the exact commit and
dependencies reviewed, the scope, the findings, the limitations and every finding
still open. It states that an assessment took place, not that the registry guarantees
the agent.

Model prices, where a registry shows a money estimate of model use, are shown with
the price list and date used, never as a fixed property of the agent.

## 8. Publishers, provenance and revocation

**Identity.** A registry account authenticates the publisher; `publisher.id` names
that account and a listing shows it. The first version uses the code host account
(`github:OWNER`) that owns the repository, proven by the registry's sign-in, so the
account that publishes is the one that controls the source. The security contact must
be confirmed by the publisher before the first listing. Ownership can be transferred
only by both parties, and revoked by the registry for abuse; both are recorded on
the agent's history.

**Revocation and supersession.** When a vulnerability is reported in a listed
version:

1. The registry records the report privately and investigates with the publisher.
2. If confirmed, the affected versions are flagged on their listings, with the
   advisory once it is public.
3. Their Verified or Independently reviewed designation is suspended while the
   finding is open, and revoked where the finding shows it should not have been
   granted.
4. Businesses are told through advisories they subscribe to: a feed per agent, and
   `staalc agent audit`, which checks the installed agents of a host against the
   registry's advisories. The registry does not track who installed what.
5. A fixed version is published and goes through the checks again; its designations
   are its own.

## 9. Compatibility

- `manifest` names the specification version. A `staalc` refuses a version it does
  not know, so a newer manifest is never read by an older tool with older meanings.
- A field's security meaning never changes within a manifest version. A change of
  meaning is a new version, and the old version keeps its old meaning for as long as
  it is supported.
- Every manifest version states the oldest `staalc` that implements it, and a
  registry states which versions it accepts.
- A capability that is deprecated stays checkable for at least one minor version of
  `staalc`, with a warning at check and install, before an agent using it is refused.

## 10. Not in this version

- **Human approval before an action.** Staal has no approval step yet; the only human
  in the loop is the operator who resolves a run stopped in doubt. An approval
  capability, journaled like any action, comes first.
- **Agent-to-agent delegation.**
- **Hosted execution.** The business runs the host; a hosted service holding
  businesses' credentials is a separate product needing its own security review.
- **Signed listings.** Until then a listing rests on the registry's authentication of
  the publisher and the pinned commit.
- **Certifying agents not written in Staal**, federation between registries,
  reputation scores, and any token or currency.

## 11. Decisions of the first review

| Question | Decision |
| --- | --- |
| Suggestions above the program ceiling | Refused |
| Grants wider than the declared reach | Refused by default; an explicit override with a reason, recorded and shown |
| Data handling declaration | Required, expanded, and labelled publisher supplied, not compiler verified |
| Who runs Verified checks | The registry, reproducibly, in isolated environments |
| Who grants Independently reviewed | Reviewers the registry recognises by published criteria |

Added by the review: publisher identity (section 8), declared and effective authority
kept apart (sections 5 and 6), failure behaviour in three parts and model use apart
from external expenditure (section 5), revocation and supersession (section 8), and
compatibility (section 9).

Two refinements of the review's proposals, for the next review:

1. **Overrides belong to whoever runs the host.** The review proposed an enterprise
   administrator override. A business installs on its own host, so whoever installs
   is the administrator; v0.1 makes the override explicit, reasoned and recorded rather
   than tying it to a role Staal does not have. Roles arrive with hosted or
   organisation features, if ever.
2. **Notifying businesses without tracking them.** The review proposed notifying
   businesses with affected deployments. v0.1 does this with subscribed advisories and
   a local `staalc agent audit` rather than a registry record of every installation,
   which would be a list of who runs what: valuable to an attacker, and not needed.

The review's own example of a wider grant, `https://*.example.com`, is already refused
by the runtime, since a `*` may not stand for any part of a host. Widening is still
possible within a host (`https://bank.example.com/*` for a declared
`https://bank.example.com/refunds`), and that is what the override governs.
