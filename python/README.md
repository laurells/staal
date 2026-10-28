# staal

[Staal](https://github.com/laurells/staal) is the programming language for agents,
built for what they do: sending messages, moving money, changing records. Its compiler
refuses a program that could act without permission, use a model's answer
unchecked, let untrusted text instruct a model, or overspend its budget, and its
runtime journals every action so a crashed run resumes without repeating one.

This package installs the `staalc` compiler and runtime, and a Python interface to
it. No Rust toolchain is needed.

    pip install staal
    staalc check program.staal
    staalc run program.staal

From Python, through Staal's C interface. Your app decides the policy (here, a
refund limit per customer tier) and Staal enforces it:

    import staal
    lib = staal.Library()          # the library installed with this package
    limit = 500 if customer["tier"] == "gold" else 50
    out = lib.run("refund.staal", {
        "entry": "main",
        "inputs": {"complaint": complaint},
        "grants": {"pay": {"cap": "payments.refund", "each": {"amount": str(limit), "currency": "USD"}}},
        "model": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": {"env": "ANTHROPIC_API_KEY"}},
    }, journal="refund.journal")
    if out["status"] == "done":
        print(out["result"])

Every call returns the JSON object described in
[include/staal.h](https://github.com/laurells/staal/blob/main/include/staal.h), as a
dict. `staalc build` also needs a C compiler (`cc`) to link executables.

**Platforms.** Python 3.8 or newer, on Linux (x86_64 and arm64, glibc 2.28 or newer)
and macOS (Apple Silicon, macOS 11 or newer; Intel, macOS 10.12 or newer), from
0.24.0. Version 0.23.0 has Linux x86_64 and macOS 15 wheels only. Elsewhere, build
from source.

**Status.** A preview: tested on all four platforms above, with no independent
security review yet.
The [tutorial](https://github.com/laurells/staal/blob/main/docs/TUTORIAL.md) takes
about an hour.

**Licence.** Apache-2.0 WITH LLVM-exception. The package contains staalc and its Rust
dependencies, whose licences are in `LICENSE-THIRD-PARTY.txt`.
