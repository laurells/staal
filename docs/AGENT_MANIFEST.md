# Staal Agent Manifest Specification, revision v0.1.5 (freeze candidate, pending implementation proof)

**Status: a design, not built.** Nothing here exists in `staalc` yet except what it
says it reuses. It is written to be reviewed before any of it is built, as
[MCP.md](MCP.md) section 12 was. v0.1 took in the first review's decisions (section
12); v0.1.1 to v0.1.5 resolve the later reviews' points without widening the scope
(sections 13 to 15, 17 and 18 list each and where it is answered). Section 16 lists what must
be built and demonstrated before the format is frozen; from v0.1.5 the remaining
blockers are closed by building those, not by further revisions of this text.

**Revision and format version are separate.** v0.1.5 is a revision of this
document. The manifest format it defines is version `0.1`, the value of `"manifest"`.
Review revisions do not change the format version; it is fixed when the specification
is frozen and first implemented, and changes after that follow section 9.

An agent manifest describes one deployable agent: what it does, what it can touch,
the most it can do, and what happens when it fails. A business uses it to decide
whether to run the agent, and a registry or marketplace uses it to list the agent,
without either having to read the code.

## 1. Three sources of truth, never merged

| Source | Says | Written by | Trust |
| --- | --- | --- | --- |
| **Description** (`agent.json`) | what the publisher claims the agent does | the publisher | a claim |
| **Computed description** (`agent.described.json`) | what the program is capable of doing | `staalc`, from the compiler-checked program and its resolved dependencies (no executable exists at publication) | as strong as the compiler and runtime |
| **Execution record** (journals and logs of the business's own host) | what actually happened | the runtime, while it runs | a record |

Every screen that shows an agent says which source each statement comes from. No
statement moves from one source to another: a description cannot claim what only the
computed description can establish, and neither speaks for what happened in a run.

Four views of authority and deployment are kept apart in the same way. Only two of
them are authority; the other two are statements:

- The **program ceiling** (authority): for each grant, the limits written in the type
  of the entry parameter or library need it is granted to (`each`, `max`, `uses`,
  `only`). The runtime narrows the grant to them when the run starts.
- The **effective grant** (authority): what the business authorises at install,
  narrowed to the ceiling.
- The **declared reach** (a statement): the targets the publisher says a tool needs
  (`reach`, section 4). Not a limit anything enforces.
- The **suggestion** (a statement): limits the publisher recommends. The business may
  grant less or more than the suggestion, provided the effective grant never exceeds
  the program ceiling. The suggestion has no authority of its own and is not a second
  ceiling.

Three invariants hold, each over things of one kind:

1. **Per consumer (the runtime).** For every consumer of a granted capability, the
   authority available to that consumer is no greater than the boundary the program
   and package rules declare for that consumer (the table below), in every dimension:
   money per action (`each`), money in total over the run (`max`), attempts (`uses`)
   and targets (`only`).
2. **Per grant (install).** The grant supplied at install fits within the widest
   boundary the program can exercise for it: the entry parameter's or library need's
   type, which is the maximum authority available to any consumer. It need not fit
   within the narrower boundaries of package slots: a broader grant can be available
   to the agent's own code and be narrowed where it is passed into a package. "Widest"
   means the most available to any one consumer, not that every consumer receives it.
3. **Reach (install).** The effective reach, the targets (`only` patterns) of the
   effective grant, lies within the declared reach, unless a recorded override says
   otherwise. Only the target dimension is compared.

The install page shows the widest authority the agent's own code can exercise, beside
the narrower boundaries of packages and library capabilities.

**Authority is bounded where it is used, not once per grant.** A granted capability
can be used by more than one part of the program, and each use has its own boundary,
exactly as the compiler and runtime enforce it today (RUNTIME.md section 6 and
"Authority (E108)"):

| Consumer | Its boundary |
| --- | --- |
| **Agent code**: the agent's own code, holding the entry capability | the entry capability type bounds the authority available to it. No package's manifest limits it. |
| **Package consumer**: a package function the capability is passed to | the package slot type narrows the capability passed in, and E108 keeps that type within the package's declared authority |
| **Library need**: a library capability's need (`needs post: cap net.http.post`) | the need type bounds the capability held inside that library capability's body, narrowed when the run starts; never held by the program |

So a package's declared authority protects only what flows into that package. If the
agent's own code holds `post` granted as `https://bank.example.com/*` and passes it to
a package that declares only `/refunds`, the package reaches only `/refunds` and the
agent's own code reaches all of `https://bank.example.com/*`. The computed description
lists every consumer of every grant with its boundary (section 5), and the install
page shows them as separate rows: it never presents a package's restriction as if it
limited the program's direct use.

Install checks a grant against the program ceiling, the widest boundary any consumer
can exercise, and refuses a grant beyond it rather than let the runtime quietly narrow
it, so the install page never shows authority that will not apply. An override can
widen a grant beyond the declared reach, never beyond the ceiling.

Where the program sets no limit, the ceiling in that dimension is unbounded and the
grant is the only bound. In the refund example, `post` is the need of `bank.payments`,
a capability declared in the agent's own module, not a package, and its type has no
`only`: nothing in the program limits where it can post. The install page says so in
those words, beside the declared reach.

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

`agent.described.json` is committed so it can be read without building anything,
and must be regenerated and committed again after every change to the source, the
manifests or the lock.
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
      "app_input_requirements": {
        "customer": "the id of the customer the business's own application has authenticated",
        "database": "the path of the business's orders database"
      },
      "grants": {
        "pay":  { "suggested": { "each": { "amount": "100.00", "currency": "USD" }, "uses": 1 },
                  "needs": { "post": { "reach": ["https://{{BANK}}/refunds"], "idempotency_key": true } } },
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
    `only` patterns, with placeholders the business fills at install. A placeholder
    is `{{NAME}}`, `NAME` matching `[A-Z][A-Z0-9_]{0,31}`, and stands for a whole host
    name and nothing else (`https://{{BANK}}/refunds`). The business supplies a host
    name; it is substituted, and the pattern is then checked as any `only` pattern is.
    A value that is not a host name, or that holds `/`, `*`, `@`, `:` or a space, is
    refused. Text outside `{{` and `}}` is literal; there is no escape, since `{` and
    `}` cannot occur in a canonical URL pattern.
    Every parameter must appear, and only parameters may.
  - `quota.suggested`: a suggested quota; the business sets the real one.
  - `app_input_requirements`: for every app input, what the business must supply.
    Required for each one, because an app input is trusted by the program: the
    agent's restriction to "the signed-in customer" holds only if the business really
    supplies an authenticated identity there (section 5, Inputs).
- `data`: the **data handling declaration**, required. Every surface shows it under
  the heading "Data handling declaration: publisher supplied, not compiler verified".
  `reads`, `writes` and `sends` describe intended data flow; `categories`,
  `processors`, `retention`, `model_training` and `regions` are operational and
  contractual statements Staal cannot check. A business holds the publisher to them;
  Staal does not claim to.

## 5. `agent.described.json`: what `staalc` computes

For the agent, and for each tool:

- **Identity**, four items, each named for what it is:
  - *Source program fingerprint*: identifies the source, not a binary, and is the
    program hash every run's journal binds (RUNTIME.md section 11). The rule: if
    changing a file can change what the agent executes or what authority it can
    exercise, the change changes the fingerprint. Its inputs, exactly:
    1. every module of the program, the agent's and its packages', by module path, with
       its text;
    2. the project's `staal.json` and `staal.lock`, as bytes;
    3. for each package: its name, its git location, its commit, and one record per
       file `staal.lock` records for it: the file's path and its SHA-256, so content is
       bound to the path it sits at. The package's own `staal.json` is one of those
       files, so it is bound by its record like any other.
    Paths are as packages already require them (RUNTIME.md, Fetching): relative to the
    package root, `/`-separated, with no empty, `.` or `..` part, no backslash or
    colon, and no two differing only in letter case; a link or submodule is refused,
    so every file is a regular file and the kind needs no field. A tree that breaks a
    rule is refused before anything is computed, so two implementations cannot read
    one tree two ways.
    Nothing else in the project affects compilation or execution: the run config
    (grants, model, limits) is bound by the journal separately, as its own hash.
    `agent.json` is not an input: its deployment settings reach a run only through the
    generated tool manifest and run config, which are bound there.
    The construction: SHA-256 of UTF-8 text made of netstrings (each field its length
    in decimal bytes, `:`, the field, `,`), starting with the label
    `staal-program-v3`, then each group above with a record count, sorted by the bytes
    of the UTF-8 of module path, package name and file path; the byte-level grammar is
    in RUNTIME.md section 11. Every field is length-framed, so no two inputs produce
    the same text. Any change to the input set, the path rules or the encoding takes a
    new label (`staal-program-v4`, ...) and a new journal version. Implemented (journal
    version 3; section 16, R1 and R9). A journal of an earlier version bound a weaker
    hash (the modules' text and the packages' names, locations and commits, without
    `staal.json`, `staal.lock` or the package files' hashes), which this `staalc` no
    longer computes: such a journal is refused, with a message to finish its run on the
    `staalc` that started it, never resumed under a hash that binds less.
  - *Compiler and runtime*: the `staalc` version that computed the description.
  - *Dependencies*: each package's name, commit, every file's SHA-256 (from
    `staal.lock`) and declared authority.
  - *Build configuration*: nothing is compiled ahead of time for an agent; the host
    compiles the source when it starts, for the machine it runs on, and native code is
    an execution of the same program, tested to give identical results to the
    interpreter. The target and engine a deployment uses are recorded in its generated
    manifest and in Verified evidence, not in the description, which is the same on
    every platform.
- **Consumers**: for every grant, each consumer and its boundary, as section 1's table
  says: the agent's own code, each package slot the capability can be passed to, and
  each library need. Each record names the grant (an entry parameter, or a need of a
  library capability granted to one), the consumer, and its boundary per dimension,
  with the source of each limit: the entry type, a package slot type, or a need type.
  **Incomplete analysis.** If the checker cannot establish every consumer of a
  capability, or the boundary at any reachable use (a capability stored or returned
  through an abstraction, passed through a function value, re-exported, or passed on
  by one package function to another beyond what it resolves), that capability's
  consumer analysis is marked incomplete. An agent with any incomplete authority
  analysis cannot be Verified, and the install page shows the unresolved boundary and
  never presents the consumer list as exhaustive. The agent's own boundary, the entry
  type, is enforced by the runtime either way.
- **Interface**: input and output JSON Schemas, as `staalc schema` gives them. When
  `staalc schema` gains capability limits (R3), it reports each limit with its source,
  so the limits declared on an entry parameter are never confused with those a
  package slot imposes.
- **Inputs and their provenance**: for each input of each tool, where its value comes
  from, as the host establishes it:
  - *Caller argument*: sent with each call by the MCP client or agent framework, so
    in effect chosen by the model and everything it read. Untrusted: it can be any
    value its type allows. Arguments never become grants, app inputs or scopes
    (MCP.md section 5).
  - *App input*: **host-supplied, not caller-controlled**. The business's
    application supplies it with each call; a call that names one in its arguments is
    refused (MCP.md section 11). That is all Staal establishes about it. Its
    correctness is the business application's responsibility, stated in
    `app_input_requirements` and shown at install as an obligation, not a guarantee:

    | Property | Established by |
    | --- | --- |
    | The input came from the application host | the Staal host |
    | The input was not supplied by the model or its caller | the Staal host |
    | The customer is actually authenticated | the business application |
    | The customer may access the record asked for | the business application and its data-access controls |
  - *Token claim*: over the HTTP transport, taken from a verified access token's
    claim (MCP.md section 12.6). As trustworthy as the authorization server the
    business configures.
  Inside the program, a model's output is `Proposed<T>` until it is validated
  (E001); that is the compiler's guarantee and needs no manifest field. Validated
  means the program's validation condition passed, not that what the model said is
  true; the developer documentation keeps that distinction visible. A tool whose
  security depends on an app input says so in `app_input_requirements`. This version
  computes provenance only: whether a grant's targets are derived from an app input is
  not computed, and the install page says that the dependency is stated by the
  publisher. Computing it would be a data-flow analysis in the checker, a later
  version's work.
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
- **External expenditure ceiling**: a bound on the external expenditure the runtime's
  capability accounting permits in one run, per currency. Not an estimate of actual
  spend. `max` is the most money spent through a capability in total over the run, and
  `each` the most one action may spend (RUNTIME.md section 6), so the bound is the sum
  over the tool's money-limited capabilities of `max`, or of `each` times `uses` where
  there is no `max`: the formula the host already uses to reserve a quota (MCP.md
  section 6). Amounts in different currencies are listed separately and never added.
  If a capability has `each` and neither `uses` nor `max`, the number of permitted
  actions has no finite bound and the result is **unbounded**, never a guess. Shown
  apart from model use: a tool with no model calls can still move money.
- **Failure behaviour**, per action, in three separate parts:
  - *Runtime recovery*: what Staal does after a crash: replays a completed action,
    resends an action in doubt with the same idempotency key, repeats an action that
    is safe to repeat, or stops the run for an operator. This is Staal's guarantee.
  - *External idempotency*: whether the grant relies on the service to deduplicate
    (`idempotency_key`). This is the **service's** guarantee, not Staal's, and is
    shown as such: Staal sends the same key; the service must honour it.
  - *Outcome certainty*: whether the action's result can be unknown after a crash,
    and if so that the run stops for an operator rather than guessing.
- **Network reach, precisely**: a grant's `only` is checked against the canonical URL
  of every request; no redirect is followed (RUNTIME.md, HTTP); outgoing TLS is
  verified against the host name. Staal enforces the canonical host name and the TLS
  boundary. It does not enforce where that name resolves: a request goes to whatever
  address the machine's DNS gives, and DNS is part of the machine, which is trusted
  (GUARANTEES.md section 2). A grant of `https://api.example.com/*` therefore does not
  mean the service is on the public internet; on a machine whose DNS points that name
  at an internal address, the request goes there. Host-based reach is not IP-level
  network isolation; DNS integrity and network isolation are the deployment's
  responsibility, and the install page says so beside every network grant.
- **Unchecked parts**: whether a tool's entry takes a C function capability (`cap
  ffi`), directly or through a library's needs, so that a run can call C; and
  whether it can reach console input. C code a tool cannot be granted cannot run, so
  it does not count; C code a tool can be granted makes the tool unchecked, as its runs
  are marked, and the agent cannot be Verified (section 7).
- **Determinism**: the description is serialized as JSON in the JSON Canonicalization
  Scheme (RFC 8785): keys sorted, no insignificant space, numbers and strings in one
  form. Every list whose order carries no meaning (packages, capabilities, reach
  patterns) is sorted by its stated key. The program fingerprint is the one the
  journal binds. Its inputs and construction are exactly those defined under *Source
  program fingerprint* above; no second or abbreviated construction is used for
  descriptions, journals or verification evidence. Two `staalc` builds of one version
  on any platform produce byte-identical descriptions; `staalc agent check` compares
  bytes.

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

1. The agent is fetched by version and verified as packages are today: files are read
   from git's objects at the commit its listing names, never from a checkout, so
   attributes and filters cannot change a byte; links, submodules and unsafe paths are
   refused; size and file-count limits apply; and every file is checked against its
   hash.
2. The installing `staalc` satisfies `requires_staalc`, implements the manifest
   version, and recomputes the description from the fetched source: it must equal
   the published one byte for byte, or install stops (section 9).
3. The business sets every grant's limits and fills every reach placeholder. A grant
   beyond the program ceiling is refused (section 1).
4. **A grant wider than the declared reach is refused.** It can be granted only with an
   explicit override naming each wider pattern and a written reason
   (`"wider_than_declared": [{"pattern": "...", "reason": "..."}]`). The override is
   recorded with its reason in the generated manifest; every run's journal binds the
   run config built from it, which holds the wider grant, so the execution record shows
   what was granted. No override reaches past the program ceiling, which the runtime
   enforces whatever the manifest says.
5. The business acknowledges each `app_input_requirements` entry: the obligations its
   application takes on.
6. Secrets are named by the business's environment variables; nothing holds a value.
7. One page is shown before anything is written: for every capability, the declared
   package authority, the program ceiling (or "unbounded by the program"), the declared
   reach, the effective grant and any override, side by side; each input with its
   provenance and the obligations acknowledged; the quotas and their scope (section
   10); the model use; three money figures, labelled as different constraints: the
   per-run external expenditure ceiling, each tool's quota-window limit, and the sum
   of the tools' quota limits, labelled as the maximum configured quota exposure, not
   predicted or actual spend; the failure behaviour in its three parts; and
   the data handling declaration under its heading.

**At run time** nothing changes: the host and broker enforce the generated manifest
as they enforce any, and the journal records the authority exercised.

## 7. Trust designations

Three, never merged, never written by the publisher, each for one version only:

| Designation | Means | Evidence shown |
| --- | --- | --- |
| **Published** | the publish checks passed, re-run by the registry | the checks' output, the commit, the `staalc` version |
| **Verified** | also passed checks the registry ran itself | below |
| **Independently reviewed** | also assessed by a registry-recognised reviewer outside the publisher | below |

**Verified** is run by the registry, never taken from the publisher, against a
**verification profile**: a versioned, published definition of the checks, kept by the
registry apart from this specification, so the standard can rise without changing the
manifest format. Every Verified designation names the profile and version it passed,
and the evidence holds the source fingerprint, `staalc` version, platform, engine and
every check's output. A profile defines:

- the test protocol: which tests are found and how they are run;
- the crash protocol: which actions are crashed at which journal points, and what a
  pass means;
- fuzzing: which targets, for how long, under which sanitizer and resource limits, and
  which exemptions are allowed and recorded;
- the compiler and runtime checks;
- the evidence format and how long evidence is kept.

Proposed **profile 1**, for review with this document:

| Check | Requirement |
| --- | --- |
| Tests | `staalc test` over the project's `tests/` passes on both engines, which it runs by default; an agent with no tests fails |
| Scenarios | at least one scenario per tool: inputs, app inputs and the stand-in services the tool calls, written by the publisher, which the registry runs |
| Crash | every scenario is crashed at every journal write, on both engines and each pairing of engines for crash and resume. What passes depends on each action's recovery contract (section 5), in the same terms: a completed action is replayed and its stand-in sees it once; an action in doubt that is resent with an idempotency key may reach the stand-in twice, and passes when the stand-in, deduplicating by that key, performed it once; an action whose outcome cannot be known stops the run for an operator, and passes when it does exactly that, with no claim that the external effect happened once; an action safe to repeat may be repeated. The resumed run's result must equal an uncrashed run's wherever the run finishes |
| Coverage report | for each scenario, the actions, capabilities and refusal or failure paths its runs exercised, read from their journals; the listing shows what no scenario exercised. A passing scenario never implies coverage it did not have |
| Fuzzing | the registry fuzzes each tool's arguments, generated from its input schema, for 10 minutes against the scenario's stand-ins, asserting no crash, no hang, and that the tool host's checks hold; the agent's own fuzz targets, if any, run too, and having none is not a failure. **Sanitizer coverage, stated with the evidence:** AddressSanitizer instruments the Rust code of `staalc` and its Rust dependencies as compiled for the fuzzer. It does not instrument the Rust standard library (precompiled, though its allocations go through the sanitizer's allocator) or C code compiled by dependencies (SQLite, ring's C and assembly). Native code generated at run time by Cranelift is not instrumented by AddressSanitizer; the profile records this limitation separately, and sanitizer results are never read as coverage of memory accesses made by generated code |
| Compiler and runtime | the publish checks; no `cap ffi` a tool can be granted |
| Execution environment | fixed by the profile and recorded in every evidence record: operating system and architecture (profile 1: Linux x86_64), the exact `staalc` release (one build serves as compiler, interpreter and native engine), the fuzzer and sanitizer toolchain versions, and the resource and time limits of each check |
| Retries | none: a scenario, crash point or fuzz run that fails once fails the check, and the evidence records it. A flaky result is a finding, not noise |
| Evidence | written by the harness in RFC 8785 JSON, kept for as long as the version is listed and 2 years after |

These numbers are proposals; their purpose is that every Verified agent passed the
same named bar. A publisher-written scenario that passes shows that scenario passed,
not that it represents the tool's risks; the coverage report keeps what was not
exercised visible on the listing.

**When the profile changes.** A new profile never rewrites old evidence or
designations. A listing shows the profile its version passed, whether that profile is
still current, and whether a newer one exists; a version re-verified under a newer
profile shows both results. A profile is withdrawn only when its checks are found to
be wrong, and designations under it are then shown as resting on a withdrawn profile,
with the reason. Publisher evidence may be shown beside the registry's, labelled, and
never replaces it. Running an untrusted agent's tests is itself an attack surface,
so the checks run in an environment made for one run and destroyed after it:

- No network but loopback. The agent's tests reach services through stand-ins they
  start themselves, as Staal's own tests do; a test that needs the internet fails.
  Fetching the source and locked packages happens before, in a separate step that runs
  none of the agent's code.
- No credentials of the registry's or of anyone's. Any secret a test needs is a fresh,
  meaningless value made for that run.
- Limits on time, memory, processes and disk; a check that exceeds one fails.
- The evidence is written by the registry's harness, outside the environment, never
  by the agent's code.

A clean fuzz run is shown as what it is: the named targets ran for the stated time
without a finding, not a general security guarantee. An agent with C code a tool can
be granted can be Published, never Verified.

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

**A host that is offline from the registry** keeps running what it installed: the
registry is not in the execution path, by design, so it has no switch to stop a
deployed agent, and a revoked version runs until its business acts. `staalc agent
audit` reports revoked and flagged versions whenever it can reach the registry; a
business that wants to be told runs it on a schedule. `staalc agent install` refuses a
revoked version, and installs a flagged one only after the business confirms it has
read the advisory. The tradeoff is deliberate: a remote switch would put the registry
in the execution path and make it a single point of failure and attack.

## 8a. Grants and their nested needs

A library capability (`pay: cap bank.payments.refund`) and each runtime capability
it needs (`needs post: cap net.http.post`) are **separate grants**, as the runtime
already treats them (RUNTIME.md, Library capabilities: "A need that is a capability is
a grant like any other"). Neither inherits limits from the other:

| Identity | What the business grants | Where the runtime enforces it |
| --- | --- | --- |
| `pay` | the library capability: money per action (`each`), in total (`max`), attempts (`uses`), targets (`only`, over the action's `@target`) | the broker, on every action the program calls through `pay`, before the action's code runs |
| `pay.post` | the need: targets (`only`, over URLs), `idempotency_key` | the broker, on every request the action's code makes through `post`, narrowed to the need's own type |

So a refund through `pay` is checked against `pay`'s limits, and every request it makes
is checked against `pay.post`'s. A restriction on `pay.post` limits where the library's
code can send requests; it is not a restriction on every possible use of `pay`, and a
money limit on `pay` is not a limit on `pay.post`, which carries no money. The install
page shows them as separate rows under their stable identities (`pay`, `pay.post`),
each with its own effective grant and consumers. The external expenditure ceiling comes
from the library capability's money limits, since the runtime counts money on actions,
whose `@amount` must reach a request (RUNTIME.md, Declaring).

In `agent.json`, `grants.pay.suggested` is the suggestion for `pay`, and
`grants.pay.needs.post` holds the suggestion and declared reach for `pay.post`.

## 9. Compatibility

Five things must agree for an installed agent to be enforced as described:

| Item | Where it is fixed | Checked |
| --- | --- | --- |
| Manifest specification version | `manifest` | refused if unknown |
| Compiler and runtime | the `staalc` version; Staal's compiler and runtime ship as one binary, so they are one version | `requires_staalc` at install; the version is recorded in the description and the evidence |
| Program | the source program fingerprint | recomputed at install and compared |
| Dependencies | `staal.lock` | every file's hash at install, as packages are verified today |
| Journal format | the journal's own version (the `version` of its start record) | a resume refuses a journal of a version it does not read |

What the journal binds is the source program fingerprint and the run config, not a
binary or a `staalc` version: a run resumed after `staalc` is upgraded resumes the same
source, under the same config, on the new version. Since every engine of every version
is held to the same results, that is the intended behaviour. A journal also records
what executed it, since recovery is Staal's guarantee. Implemented (journal version 3;
section 16, R2):

- The start record, written once and never changed, gains the `staalc` version and
  the engine that started the run.
- Each resume appends a `resume` record, flushed before the resumed run does anything:
  the run id; the resuming `staalc` version; the engine; the journal format version;
  the source program fingerprint hash and the run config hash, as the resume checked
  them; and the time. Its outcome is what the records after it show: the operations
  replayed or made, and the `end` record or the next crash.

So a run started by one version with the interpreter, crashed, resumed by a second
version with native code, crashed again and resumed by a third, shows all three in its
journal, in order.

Parsing an agent is not the same as enforcing it. A `staalc` older than
`requires_staalc` refuses the agent even if it could parse it, because it may lack a
check the description relies on. A newer `staalc` installs it, and the install page
shows when the version differs from the one the registry verified with: Verified
evidence is for the version it names.

- `manifest` names the specification version. A `staalc` refuses a version it does
  not know, so a newer manifest is never read by an older tool with older meanings.
- A field's security meaning never changes within a manifest version. A change of
  meaning is a new version, and the old version keeps its old meaning for as long as
  it is supported.
- Every manifest version states the oldest `staalc` that implements it, and a
  registry states which versions it accepts.
- A capability that is deprecated stays checkable for at least one minor version of
  `staalc`, with a warning at check and install, before an agent using it is refused.
- A journal resumes under any later `staalc` that reads its journal version, without
  repeating an action (section 16, R5). A release that stops reading a version refuses
  such a journal and says so in its notes, as journal version 3 does for versions 1
  and 2; the operator lets those runs finish first (MCP.md section 7).

**What a resumed run uses.** A run is resumed from what it was started with, never
from the current manifest (MCP.md section 7). Its execution configuration is:

| Part | Held in | Bound by |
| --- | --- | --- |
| Program path and engine | the run file, written before the journal | the program's text by the source fingerprint, checked again at resume; the engine may differ on resume, by design (both give identical results) and recorded by R2 |
| The run config: `entry`, `inputs` (the caller's arguments and the app inputs' values), `grants` with their needs, `secrets` (variable names only), `model`, `net` (including `proxy`), `fs`, `adapters`, `limits` (including the tool's deadline, as `deadline_ms`) | the run file | the journal's config hash, checked at resume |
| The tool, the scope, the `once` key and the quota reservation | the ledger's `reserve` event, written before the run | the ledger, append-only and flushed |

Editing the manifest after a run starts (its program path, entry, app input mapping,
`once`, deadline or quota) therefore changes nothing for that run: it resumes under the
configuration in its run file and reserve event, or refuses to resume if its program's
text changed. Publisher metadata from `agent.json` (title, summary, categories, data
declaration) reaches no run. Two gaps were found here and are closed in journal
version 3 (R7):

- The config hash is SHA-256 of the run config's RFC 8785 form (it was the form
  `serde_json` writes: deterministic within `staalc`, but not specified). A number an
  IEEE double cannot hold exactly is refused rather than rounded.
- `"proxy": "environment"` is read once when the run starts, recorded in the start
  record (the variable that won, the proxy's scheme, host and port, and the
  `NO_PROXY` entries), and used as recorded; a resume under a different one is
  refused. A proxy URL with a user name or password, and one this build cannot use,
  are refused (RUNTIME.md section 12, Proxies).

Corrected while checking this: MCP.md section 7 said that upgrading `staalc` makes an
unfinished run refuse to resume. The code refuses when the program, the config or the
journal's format version differs, not on every upgrade; MCP.md now says so.

## 10. Scope of quotas, `once` and money

**One deployment, one ledger.** A deployment is one tool host: one journal directory,
one ledger, held by one process through an OS lock (MCP.md section 6). Two businesses installing the same
agent have two deployments and share nothing: no quota, no `once` key, no journal. Two
deployments of one business share nothing either; a business that wants one limit
across them runs one host.

**Per tool.** A quota (calls and money per window) is per tool, across every caller of
that tool in the deployment. There is no quota across tools: the deployment's maximum
exposure in a window is the sum of its tools' money quotas per currency, and the
install page shows that sum. Over the HTTP transport a tool may also give each
principal a share of its quota (MCP.md section 12.6); that is per principal, within
the tool's quota.

**Money quotas** apply to the tool's money-limited capabilities together, in one
currency: the host refuses a money quota on a tool whose money-limited grants are in
more than one currency, or whose worst case per run is unknown (MCP.md section 6).

**`once`** keys are per tool and per scope: the app's scope for each call, or the
principal over HTTP, so two tenants of one deployment never share a key (MCP.md
sections 5 and 11). The key is made from the arguments `by` names (`order`); a
stable identity is the publisher's choice of those arguments and is shown at install.
`keep` is how long a finished operation's answer is kept: within it, the same call is
answered from the same run, including after any crash and recovery; after it, the same
call is a new operation. A run that is in doubt or waiting for an operator keeps its key
whatever `keep` says, since `keep` counts from the run's end (MCP.md section 5), so a
retry never starts a second run of an operation not yet finished.

## 11. Not in this version

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

## 12. Decisions of the first review

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

Two refinements of the first review's proposals:

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

## 13. Changes in v0.1.1 (the second review)

| Review point | Resolution | Section |
| --- | --- | --- |
| Declared reach versus package authority | Four values of authority; two invariants; an override widens beyond the declared reach, never beyond the ceiling, which includes package authority and is enforced by the runtime; an unbounded dimension is shown as unbounded | 1, 6 |
| Trusted versus model-controlled inputs | Every input's provenance computed (caller argument, app input, token claim); `app_input_requirements` states the business's obligations, acknowledged at install | 4, 5, 6 |
| Scope of quotas and `once` | One deployment, one ledger; quotas per tool, no cross-tool quota, the deployment's sum shown; money quotas in one currency; `once` per tool and scope, `keep` defined against recovery | 10 |
| Runtime compatibility | Five things that must agree; compiler and runtime are one binary; the description recomputed and compared at install | 6, 9 |
| C code | Only C a tool can be granted counts | 5 |
| Deterministic description | RFC 8785 serialization, sorted lists, fingerprint inputs, byte comparison | 5 |
| Network reach | No redirects followed; TLS verified against the host; DNS trusted as part of the machine | 5 |
| External expenditure with several actions or currencies | The host's reservation formula; currencies never added; unbounded stated | 5 |
| Verified sandbox | Loopback only, fetch separate, no real credentials, resource limits, evidence written outside | 7 |
| Revocation while offline | No remote switch, by design; `audit` on a schedule; install refuses revoked versions | 8 |

Corrected from v0.1: v0.1 said the runtime refuses a grant beyond the program
ceiling. It narrows it (RUNTIME.md section 6). Install now refuses such a grant, so
what the business sees is what applies.

## 14. Changes in v0.1.2 (the third review)

| Review point | Resolution | Section |
| --- | --- | --- |
| Authority composition | Bounded at each use, as the compiler and runtime already do: the agent's own code by the entry type, a package slot by its type within the package's authority, a library need inside its capability only; every consumer listed with its boundary; install checks against the widest; a package's restriction never shown as limiting the program's own use | 1, 5 |
| Suggestions independent of grants | The business may grant less or more than the suggestion within the ceiling; "four views", of which two are authority | 1 |
| What qualifies for Verified | A versioned verification profile, named in every designation; profile 1 proposed | 7 |
| The network boundary | Staal enforces host name and TLS, not resolution; not IP-level isolation; said beside every network grant | 5 |
| The fingerprint | Named the source program fingerprint; compiler, dependencies and build configuration recorded apart; the journal's binding stated, and recording the `staalc` version in the journal made a requirement | 5, 9 |
| Document and format versions | Stated as separate at the top | top |

Corrected from v0.1.1: it said package authority is part of one program ceiling per
grant. It is a boundary at the package's own slots, and does not limit the agent's
own code (RUNTIME.md, "Authority (E108)": "The program's own code is not limited by a
manifest").

## 15. Changes in v0.1.3 (the fourth review)

| Review point | Resolution | Section |
| --- | --- | --- |
| The fingerprint must cover every security-relevant input | Inputs enumerated (modules, `staal.json`, `staal.lock`, each package's identity, manifest and file hashes); a domain-separated, length-framed construction; a requirement, since today's construction misses the manifests and lock and frames a single-file program not at all | 5, 16 |
| An append-only resume record | The start record stays as written; each resume appends a record of version, engine, journal version and the hashes it checked | 9, 16 |
| The invariants, over sets of one kind | Effective grant within the ceiling in every dimension at every consumer boundary; effective reach within declared reach | 1 |
| Profile 1's precision | Sanitizer coverage stated, including what it does not instrument; crash passes by each action's recovery contract; a coverage report per scenario; new profiles never rewrite old evidence | 7 |
| Provenance is not correctness | App inputs called host-supplied, not caller-controlled; who establishes what, in a table; validated is not true | 5 |
| One request, one authority decision | A requirement: the URL that is checked is the one connected to, verified and sent | 16 |

Found by checking the review against the code: the existing fuzz evidence in
docs/security-review/ASSURANCE.md says `engines` ran under AddressSanitizer without
saying that native code generated by Cranelift is not instrumented. That is corrected
there too.

## 16. Required before the format is frozen

Changes to the runtime, each needed for a claim this specification makes, and each to
be demonstrated by a test that fails without it:

| | Requirement | Demonstrated by |
| --- | --- | --- |
| R1 | The source program fingerprint of section 5, in a new journal version; a journal of an older version is refused, never resumed under a weaker hash | a test that changes each input of section 5 in turn (a module, `staal.json`, `staal.lock`, a package's manifest, a package file) and sees the fingerprint change and a resume refused |
| R2 | The `staalc` version and engine in the start record; a `resume` record per resume | a test that crashes and resumes across both engines and checks every record |
| R3 | `staalc schema` reports the limits written in capability types, and every consumer of every grant with its boundary | tests over programs with entry limits, package slots and library needs, and over propagation: a capability passed through several functions, returned from one, stored in a value and read back, passed through a function value, re-exported by a package, passed from one package's function into another's, and one grant with several consumers of different limits. Each test asserts one of two outcomes: the consumer graph complete and exact, or marked incomplete with the agent refused Verified |
| R4 | A package cannot exercise more than its declared boundary while the agent's own code holds broader authority over the same grant | a runtime test in which the agent's code reaches `https://bank.example.com/x` through a grant the package also receives, and the package's attempt at `/x` is refused while its `/refunds` succeeds |
| R5 | A crash and resume across engines and across `staalc` versions never repeats an action blindly | the existing crash tests across engine pairings, plus journals of every journal version still supported, kept as fixtures from the releases that wrote them and resumed by the current one at every crash point. A **release gate for `staalc` itself**, in CI, not only a Verified check: a broken resume would affect every installed agent |
| R6 | One authority decision per request, kept through the whole request: the request target is parsed and made canonical once, into one immutable representation; the `only` check, the connection, TLS verification and the request line all use that representation, and the HTTP client never re-reads the original string. The HTTP authority (`Host`, and `:authority` if HTTP/2 is ever used), the TLS verification name (and SNI) and the checked canonical host are always the same. No proxy is used unless the deployment's config names one: today `net.http` uses none unless the run config asks for the environment's, and the model provider's `"proxy": "environment"` likewise; a configured proxy is an explicit deployment dependency, shown at install as part of the network boundary. A pooled connection is reused only for a request to the same scheme, host and port it was opened and verified for. Before R6 the URL was parsed by the `url` crate for the check and again inside the HTTP client: whether the two can disagree was part of this review | a review of `net.http`, the model provider and key fetching against this rule; adversarial tests for international names (IDNA), IPv4 and IPv6 literals, explicit and default ports, trailing dots and case, percent-encoded separators and dot segments, a proxy set in the environment and not configured, and a kept-alive connection followed by a request to another host. Each asserts that the authority the policy checked and the authority actually connected to are identical, not only that the request was accepted or refused |
| R7 | The run config hashed as RFC 8785; the proxy URL a run used recorded, and a resume under another refused (section 9) | a test that changes, between a crash and its resume, each manifest field a run depends on (program path, entry, app input mapping, `once`, deadline, quota, grants, proxy) and shows the resumed run either uses the original or refuses, never the change |
| R8 | A normative JSON Schema (2020-12) for `agent.json` and `agent.described.json`, with the semantic rules a schema cannot state, and valid and invalid fixtures. Unknown keys refused at every depth. Types fixed: money as `{"amount": decimal string, "currency": ISO 4217 code}` with at most the currency's decimal places, as the runtime reads it; durations as the tool host reads them (a whole number and `s`, `m`, `h` or `d`, per field as MCP.md allows); `requires_staalc` as a version range in the grammar packages use; an empty `suggested` as no suggestion in any dimension | the fixtures, each refused or accepted as marked, by `staalc agent check` |
| R9 | A normative fingerprint algorithm: UTF-8 bytes without Unicode normalization; the domain label `staal-program-v3` as the first framed field; every group framed with a record count, every package framed as a nested record holding its own fields and file records; ordering by the bytes of UTF-8, never by locale; digests as lowercase hex, in the description and inside the construction (which is UTF-8 text, so it holds no raw bytes) | test vectors: one project, one package, several packages, nested dependencies, a changed path, a changed content, a changed manifest; a second implementation, written separately (in Python), produces the same digest for every vector. And a demonstration, against the project and package loader, that no file outside the inputs can change what compiles or runs |

**Status.** R1, R2, R7 and R9 are implemented, in journal version 3; R6 in the HTTP
clients; R4 holds by the language, now demonstrated; R5's gate is in place, with the
limit stated below; R3 in `staalc schema`. Each is demonstrated by these tests. R8 is
open.

- R1: `every_input_of_a_program_is_bound_by_its_journal` (tests/packages_test.rs)
  changes, alone, a module, the project's `staal.json` and `staal.lock` (only their
  layout), a package's `staal.json` and a package module; each gives another
  fingerprint and a refused resume, and putting it back gives the original and a
  resume.
- R2: the crash tests (tests/crash_test.rs) crash a run at every journal write and
  before every result, on the interpreter and on native code, resume it on each engine
  pairing, and check every start and `resume` record: the start names the `staalc`
  version and the engine that started the run, each resume of the unfinished run is
  recorded (and none of a finished one) with its run id, `staalc` version, engine,
  journal version, both hashes, proxies and time.
  `every_runtime_case_is_identical_on_both_engines` and
  `every_runtime_case_is_identical_as_an_executable` require each start record to name
  the engine that wrote it.
- R7: `a_resumed_run_never_takes_a_change_to_the_manifest` (tests/mcp_crash_test.rs)
  changes, alone, between a crash and the resume, the program path, the entry, `once`,
  the deadline, the quota, the grants (`only`, then `each`) and the proxy; each time the
  resume record binds the same program, config and proxies as the start, and the run
  refunds the original amount once. `a_resumed_run_keeps_its_app_inputs_when_the_mapping_changes`
  (tests/mcp_http_test.rs) does the same for the app input mapping.
  `a_run_file_pointed_at_another_program_is_refused` shows a run file pointed at other
  text is refused and the same text at another path resumes.
  `a_run_resumes_only_under_the_proxy_it_started_with` refuses a resume under another
  proxy, another `NO_PROXY`, a variable read before the recorded one, and none, and a
  proxy URL holding a credential, without showing it. The RFC 8785 serializer is
  checked against Node.js 22 on 6,119 committed vectors (src/jcs.rs).
- R9: `scripts/fingerprint.py`, written from the grammar alone, produces the 19 vectors
  in `tests/fixtures/fingerprint/vectors.json` (no project, an empty file, non-ASCII
  paths and text, an empty lock, a changed manifest, one package, several packages
  given out of order, nested dependencies, a file at another path, other content,
  another commit, a package with no files, module paths and package names that differ
  only in order), and `staalc` agrees on every one. `no_file_outside_the_inputs_changes_what_compiles`
  (tests/packages_test.rs) is the demonstration against the loader.
- R6: a URL is parsed once into a target (`targets::Target`, src/targets.rs) that
  cannot change; the `only` check uses its canonical text and the HTTP client is given
  the URI built from the same parsed parts, refused if the HTTP layer would read it
  back as anything else. The double parse named in the requirement is gone: the
  client never parses the program's text. The review of the three clients found the
  same double parse in the model provider (its `base_url` was checked parsed, then
  sent as the text given) and in key fetching; both now use a target. It also found
  that `net.http` looked the host up itself before sending even through a proxy,
  which would refuse every request where only the proxy can resolve names; it no
  longer does. ureq takes the connection's address, `Host`, the TLS name and SNI, the
  `CONNECT` destination and the request line from that one URI, and keys its
  connection pool by scheme, authority and proxy (ureq 3.4.2, `pool.rs`). Tests:
  `the_uri_sent_is_the_target_checked` (src/targets.rs) over international names,
  full-width dots, IPv4 in hexadecimal, octal, short and integer forms, IPv6 and
  IPv4-mapped literals, default, explicit and zero-padded ports, trailing dots and
  case, encoded dot segments and separators, a backslash before an `@`, tabs and
  newlines; `the_request_sent_is_the_target_checked` (src/interp/http.rs) sends such
  spellings to a local server and compares the request line and `Host` it receives;
  `a_kept_alive_connection_is_used_only_for_its_own_authority` shows the pool reused
  for one authority and never for another reaching the same address;
  `a_proxy_is_asked_for_the_checked_host_and_port` reads the `CONNECT` line and its
  `Host` at a proxy; `a_proxy_in_the_environment_is_not_used_unless_the_config_asks`
  (tests/http_test.rs and tests/claude_test.rs) and
  `keys_are_never_fetched_through_a_proxy_in_the_environment` (tests/mcp_http_test.rs)
  set every proxy variable and see no connection to the proxy; each fails when its
  client is built without `.proxy(None)`. `a_library_cannot_set_host`
  (tests/http_test.rs) refuses `Host`, `:authority` and `proxy-` headers set from a
  library.
- R3: `staalc schema` reports each grant's `limits` (its program ceiling) and
  `consumers`: for every grant, the agent's own code, each package slot it can be
  passed to from outside that package, and each library need, with the boundary each
  is held to and its source (RUNTIME.md section 1). The analysis (src/consumers.rs)
  follows parameters, `let`, return values and needs, per assignment of grants to a
  function's parameters, and recursion to a fixed point; it reports anything it cannot
  follow under `consumer_analysis.unresolved` and marks itself incomplete.
  `every_consumer_of_every_grant_with_its_boundary` (tests/consumers_test.rs) checks
  the exact list for a project where grants pass through several functions, are
  returned from the agent's function and from a package's, go through a package into
  another package (and a package's own function, which is not a consumer), are called
  by a name imported from a package, and are needs of library capabilities, the
  agent's and a package's, one handing its need to a package; one grant reaches four
  consumers with different limits. Other tests cover entry limits, a function that
  returns either of two grants, and recursion. Storing a capability in a value,
  carrying it in a function value and reassigning it are not in the language (the
  tests assert the checker refuses them), and the language has no re-export, so those
  cases of the requirement cannot arise; a package that wraps another's function is
  the case that can, and is tested. The incomplete outcome, which no accepted program
  produces, is tested on a program altered by hand. Verified does not exist yet; when
  it does, it must refuse an agent whose analysis is incomplete.
- R4: a capability can travel only as a parameter, a typed `let`, a return value or a
  library's need: it can be held in no list, record, tuple, optional, map or function
  value, and never reassigned (E101, E003). Each of those entries narrows it to the
  slot's declared limits at run time (a child capability that counts against every
  ancestor), and a package must declare limits within its manifest at every one
  (E108). So a package holds only children within its boundary, whatever the agent
  holds. A slot wider than the capability flowing into it is refused at check time.
  `a_package_is_held_to_its_boundary_when_the_agent_holds_more` (tests/packages_test.rs):
  the agent holds `net.http.post` without limits and posts to `/ok` itself; handed
  directly, through a function of its own, through the package into a second package,
  back out as a return value, and as a library need the operator granted without
  limits, the package's `/v1/refunds` succeeds and its `/ok` is refused, nothing sent,
  on both engines; and a program in which the package passes its capability to the
  second package's wider slot does not check.
- R5: `tests/journal_compat_test.rs` resumes the journals released binaries wrote,
  kept in `tests/fixtures/journals/`, at every crash point (each journal write, and
  between each operation and its result) and on both engines; a journal of a version
  the build reads must end with the release's output and exactly its world, and one of
  any other version must be refused with nothing changed. `release.yml` runs it before
  every release. The crash states are derived from each release's uninterrupted run,
  since a released binary has no crash injection; `derived_crash_states_are_the_states_a_crash_leaves`
  (tests/crash_test.rs) shows, for the current build, each derived state equal byte
  for byte (apart from the run id and the config hash) to what a real crash at that
  point leaves. The limit today: no release has written journal version 3 yet, so the
  committed fixtures (from 0.23.0 and 0.27.0, version 2) show refusal only; the
  resume half was run against the current build's own journals (420 crash states
  across both engines) and starts gating releases when the first version 3 release's
  journals are recorded. `UNRELEASED` in that test marks version 3 as written by no
  release yet; the test fails if its journals are added while it is set, and, once it
  is cleared, if none are present. A set's `staalc` must match its name. No release before 0.23.0 published a Linux
  binary, so there is no version 1 fixture. Scenarios with random numbers and the
  clock are excluded from the fixtures, since a resumed run draws new values after the
  crash point; tests/crash_test.rs covers them on the current build.

**Verified never transfers.** A Verified designation is shown with the exact
`staalc` release and platform it was earned on, as a historical fact about that
combination. A business on another release or platform sees that its own environment
was not the one verified; no part of a listing may imply otherwise.

These test Staal's execution model, not only the marketplace. They are worth doing
whether or not the marketplace is built.

## 17. Changes in v0.1.4 (the fifth review)

| Review point | Resolution | Section |
| --- | --- | --- |
| The fingerprint defined twice, the second time incompletely | One definition; Determinism refers to it and forbids any other | 5 |
| Package files bound by hash alone | One record per file: path and SHA-256; path rules stated; links and submodules refused, so no kind field is needed | 5 |
| Versioning of the fingerprint algorithm | Any change to inputs, path rules or encoding takes a new label and journal version | 5 |
| The authority invariant mixed a grant with consumer authority | Three invariants: per consumer (runtime), per grant against the widest boundary (install), reach (install); "widest" defined | 1 |
| R6 incomplete | Host, TLS name and canonical host kept the same; proxies only when configured, shown at install; pooled connections only for the same origin; the double URL parse named for review | 16 |
| "From the compiled program" | From the compiler-checked program and its resolved dependencies | 1 |
| Consumer analysis that cannot be completed | Marked incomplete, never shown as exhaustive, and bars Verified | 5 |
| What the expenditure ceiling bounds | `max` per run and `each` per action stated; a bound, not an estimate; unbounded when no finite bound exists; three money figures kept apart at install | 5, 6 |
| The sanitizer claim about generated code | Stated only as a limitation: generated code is not instrumented | 7 |
| Profile execution environment | Platform, `staalc` release, toolchains and limits fixed and recorded; no retries | 7 |
| Smaller points | Description regenerated on every change; consumer records name grant, consumer and limit source; schema reports each limit's source; install's fetch rules spelt out; resume tests over every supported journal version; quota sum labelled as configured exposure | 3, 5, 6, 16 |

The data handling declaration stays a declaration: a complete `data` object is never
shown as a check mark or a verified privacy claim.

## 18. Changes in v0.1.5 (the sixth review)

| Review point | Resolution | Section |
| --- | --- | --- |
| The journal-bound run configuration | Defined: run file (program path, engine, run config with every key listed) plus the ledger's reserve event; a resumed run never reads the current manifest; two gaps made R7 (RFC 8785 config hash, the proxy URL recorded) | 9, 16 |
| An unambiguous fingerprint encoding | Made R9: a normative algorithm with test vectors and a second, independent implementation, plus a demonstration against the loader | 16 |
| Grants and nested needs | Separate grants with stable identities (`pay`, `pay.post`), neither inheriting from the other; where each is enforced | 8a |
| R6's single parsed representation | One immutable parsed target used by check, connection, TLS and request; adversarial tests that compare checked and connected authority | 16 |
| A normative manifest schema | Made R8, with the types fixed now; placeholders defined (`{{NAME}}`, a whole host only) | 4, 16 |
| Verified across compiler versions | Never transfers; shown with release and platform; R5 a release gate for `staalc` | 16 |
| Consumer analysis under propagation | R3's tests list the hard cases and assert one of two outcomes | 16 |

Found while checking the review against the code, and corrected in MCP.md: section 7
said an upgrade of `staalc` stops unfinished runs from resuming. The code stops them
when the program, the config or the journal format version differs.

**Why the next step is code, not v0.1.6.** The last two reviews found no
architectural problem; their findings are precision that only an implementation can
settle: a schema that parses, an encoding with vectors two implementations agree on,
tests that pass or fail. R1 to R9 are where those are closed. The format is frozen
when they pass, with whatever corrections building them forces into this text.

### Conditions carried into implementation (the seventh review)

The seventh review approved v0.1.5 as the implementation baseline. These are its
conditions, recorded here rather than in a new revision; each is part of the
requirement it names and must hold before the format is frozen.

- **R7, proxies.** Bind the effective proxy configuration of every subsystem that
  makes requests (`net.http`, the model provider, key fetching): every variable read
  (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` in both cases, and `NO_PROXY`; RUNTIME.md
  said only `HTTPS_PROXY` and is corrected), and which one won. A proxy URL holding a
  user name or password is refused, since its credentials would otherwise be written
  to the journal; credentials for a proxy, if ever supported, come through a secret
  reference like any secret. What is recorded is the proxy's scheme, host and port and
  the `NO_PROXY` list; a resume under any other is refused.
- **R7, program path.** A test shows that pointing a run file at another file cannot
  run different source under the original fingerprint. (A different path holding
  different text changes the fingerprint and is refused; the same text at another
  path is the same program.)
- **`once` scope.** The scope is the value the app passes with each call (the C
  interface's `scope` argument, the Python and Node.js `scope` parameter) or, over
  HTTP, the principal's HMAC (MCP.md 12.6); it is stored in the ledger's `reserve`
  event with the key. The app must pass the same scope for retries of one operation;
  that is its obligation, stated at install with `app_input_requirements`. A test:
  two tenants submit the same order, then each retries, and each retry gets its own
  tenant's run.
- **R6, request authority.** A program cannot set `Host` (or any `proxy-` header): the
  runtime refuses them today (`http.rs`, `reserved_header`). Tests add: a library that
  tries to set `Host` to another host, and a proxy whose `CONNECT` destination must be
  the checked host and port.
- **R8, money.** The runtime's reading of money is the single source of truth, and R8
  states its grammar exactly: digits with an optional `.` and at most the currency's
  minor-unit digits; no sign, exponent, leading `+`, spaces or grouping; whether a
  leading zero or `"100"` for `"100.00"` is accepted, as the runtime does; the
  ISO 4217 table and revision the runtime uses, with zero- and three-digit currencies
  among the fixtures; unknown codes refused.
- **R9, encoding.** A byte-level grammar or normative pseudocode, not prose, with
  vectors for empty groups, empty files, non-ASCII paths and names that differ only in
  order.
- **Wording.** Declared package authority is a compiler-enforced boundary on a package
  consumer, not an install-time grant; section 5's "authority, in two layers" is read
  that way.
- **Publisher identity.** The registry establishes that the publishing account may
  publish for the repository (its owner, or authorised by an organisation that owns
  it), not only that it signed in.
- **Fuzzing.** Profile 1's argument fuzzing tests valid inputs; the host's own
  protocol and parser fuzzing (malformed JSON, bad encodings) is a separate target of
  Staal's, not of the agent's.
