# Staal documentation

## Learn

| Document | Contents |
| --- | --- |
| [TUTORIAL.md](TUTORIAL.md) | From install to a program that sends messages and asks a model, step by step; every program and output is checked by the tests |
| [frameworks/](frameworks/) | A support agent that refunds within the app's limits, on five agent frameworks: [OpenAI Agents SDK](frameworks/openai-agents.md), [LangGraph](frameworks/langgraph.md), [Vercel AI SDK](frameworks/vercel-ai.md), [Claude Agent SDK](frameworks/claude-agent-sdk.md), [Google ADK](frameworks/google-adk.md) |
| [PACKAGES.md](PACKAGES.md) | Writing a package: one service, from an empty folder to passing tests |
| [../examples/](../examples/) | Programs for real services, run in the tests against local stand-ins, and the finance agent attacked on purpose |

## Reference

| Document | Contents |
| --- | --- |
| [GUIDE.md](GUIDE.md) | The language: the safety rules, syntax and types, capabilities, models and budgets, modules and packages, native code, and the tools (`staalc fmt`, `test`, `doc`, `repl`, `debug`, `profile`, `lsp`) |
| [RUNTIME.md](RUNTIME.md) | The runtime specification: values, statements, model calls, capabilities, budgets, failures, the log, the journal and resuming, the runtime's own capabilities, packages, engines, the C interface |
| [GRAMMAR.md](GRAMMAR.md) | The complete syntax, checked against the parser by the tests |
| [../include/staal.h](../include/staal.h) | The C interface |

## Tools, integrations and guarantees

| Document | Contents |
| --- | --- |
| [MCP.md](MCP.md) | Programs as tools: the MCP server's manifest and protocol, quotas, `once`, app inputs, crashes and operators, and the same tools in an app (C, Python, Node.js) and in agent frameworks |
| [GUARANTEES.md](GUARANTEES.md) | Each guarantee, what it depends on, how it is tested and where it stops |
| [../python/README.md](../python/README.md), [../node/README.md](../node/README.md) | The Python and Node.js packages |

## Project

| Document | Contents |
| --- | --- |
| [../CHANGELOG.md](../CHANGELOG.md) | What changed in each version |
| [DEFERRED.md](DEFERRED.md) | Work decided on and put off, and why |
| [RELEASING.md](RELEASING.md) | How a release is made, and what it holds |
| [../CONTRIBUTING.md](../CONTRIBUTING.md), [../SECURITY.md](../SECURITY.md) | What a change needs; reporting a vulnerability |
