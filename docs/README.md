# Staal documentation

Every program, command and output on the pages under `get-started/`, `guides/` and
`reference/` is checked by the test suite (`tests/docs_test.rs`).

## Get started

| Page | Contents |
| --- | --- |
| [Install](get-started/install.md) | Supported platforms; installing `staalc`, the Python and Node.js packages, or building from source |
| [Your first program](get-started/first-program.md) | Write, check, run, test and format a program that acts through a capability |
| [Your first agent](get-started/first-agent.md) | An agent with a model and a payment capability: tests, a scripted model, Claude, and a publishable folder |

## Guides

| Page | Contents |
| --- | --- |
| [Capabilities](guides/capabilities.md) | Limits, grants and narrowing; the runtime's own capabilities and HTTP; library capabilities |
| [Models](guides/models.md) | `propose`, validation, trust, keeping data from a model, budgets, providers |
| [Journals and resuming](guides/durability.md) | Crash-safe runs, actions in doubt, `staalc resolve`, `Done` and evidence |
| [Concurrency](guides/concurrency.md) | `parallel`, limited capabilities, crashes in tasks |
| [Files, databases and sockets](guides/files-databases-sockets.md) | `fs`, `db.sqlite`, `db.postgres`, TCP and TLS exchanges, listening |
| [Calling C functions](guides/c-functions.md) | `foreign` declarations, grants, isolation, unchecked runs |
| [Native code and executables](guides/native-code.md) | The native engine, which functions qualify, `staalc build` |
| [Packages](guides/packages.md) | Dependencies, `staal.lock`, verification, writing a package, registries |
| [Calling Staal from an app](guides/apps.md) | Python, Node.js and C; results; tools; agent frameworks |
| [Serving tools over MCP](guides/mcp.md) | `staalc mcp`: manifests, `once`, quotas, operators |
| [Publishing an agent](guides/agents.md) | `agent.json`, `agent.described.json`, `staalc agent check` |
| [Agent frameworks](frameworks/) | The same support agent on the [OpenAI Agents SDK](frameworks/openai-agents.md), [LangGraph](frameworks/langgraph.md), [Vercel AI SDK](frameworks/vercel-ai.md), [Claude Agent SDK](frameworks/claude-agent-sdk.md) and [Google ADK](frameworks/google-adk.md) |

## Reference

| Page | Contents |
| --- | --- |
| [Command line](reference/cli.md) | Every `staalc` command and its exit codes |
| [Error codes](reference/errors.md) | Each check error, with an example |
| [Run config](reference/run-config.md) | Every key of a run config |
| [Language](reference/language.md) | Declarations, types, literals, statements, functions, failures, modules |
| [Standard library](reference/standard-library.md) | Built-in functions |
| [GRAMMAR.md](GRAMMAR.md) | The complete syntax, checked against the parser |
| [../include/staal.h](../include/staal.h) | The C interface |

## Specifications

| Document | Contents |
| --- | --- |
| [RUNTIME.md](RUNTIME.md) | The runtime: values, statements, model calls, capabilities, budgets, failures, the log, the journal, the runtime's own capabilities, packages, engines, the C interface, executables, concurrency, C libraries |
| [MCP.md](MCP.md) | The MCP server: manifest, protocol, HTTP transport, quotas, `once`, operators |
| [AGENT_MANIFEST.md](AGENT_MANIFEST.md) | The agent folder format |
| [PACKAGES.md](PACKAGES.md) | Building a package for a real service, with tests against a local stand-in |
| [GUARANTEES.md](GUARANTEES.md) | Each guarantee, what it depends on, how it is tested and where it stops |
| [security-review/](security-review/) | Material for an independent security review |

## Project

| Document | Contents |
| --- | --- |
| [../CHANGELOG.md](../CHANGELOG.md) | Changes in each version |
| [DEFERRED.md](DEFERRED.md) | Work decided on and put off, and why |
| [RELEASING.md](RELEASING.md) | How a release is made |
| [../CONTRIBUTING.md](../CONTRIBUTING.md), [../SECURITY.md](../SECURITY.md) | Contributing; reporting a vulnerability |
| [../examples/](../examples/) | Programs for real services, run in the tests against local stand-ins |
