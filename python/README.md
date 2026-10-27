# staal

[Staal](https://github.com/laurells/staal) is a programming language for software
where AI acts: sending messages, moving money, changing records. Its compiler
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

**Platforms.** Linux on x86_64 with glibc 2.28 or newer. Other platforms come
later; until then, build from source.

**Status.** A preview: tested on Linux only, with no independent security review yet.
The [tutorial](https://github.com/laurells/staal/blob/main/docs/TUTORIAL.md) takes
about an hour.

**Licence.** Apache-2.0 WITH LLVM-exception. The package contains staalc and its Rust
dependencies, whose licences are in `LICENSE-THIRD-PARTY.txt`.
