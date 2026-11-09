# Fuzzing staalc

Coverage-guided fuzzers (libFuzzer, through `cargo fuzz`) for the parts of staalc
that read input from outside: program text, run configs, journals, package files
and versions. Each asserts that nothing panics or hangs, and some check properties
on top.

| Target | Input | Checks |
| --- | --- | --- |
| `parse` | any text as a program | lexing, parsing, modules, constants, the checker and budgets refuse or accept it, never panic |
| `format` | any text | the formatter refuses it, or gives text that formats to itself and that the checker judges as it judged the original |
| `journal` | a genuine start record, then any bytes | resuming the crash-test workflow on it, and recording `--retry`, fail with a message or succeed |
| `config` | any JSON as the workflow's run config (scripted model only, so nothing reaches the network) | refused with a message or run |
| `manifest` | any text | manifests, lockfiles (which must read back as themselves), registry entries, versions (likewise) and ranges |
| `jwt` | any text as an access token, and as a JWK set | refused with a rule or verified, never a panic; no input verifies against the test keys, whose private halves it does not have |

## Running

Install once: `cargo install --locked cargo-fuzz --version 0.13.1`. Seed the corpora
from the repository (programs, run configs, manifests and lock files, the JWT test
tokens) with `scripts/fuzz_seed.sh`; seeding again adds nothing already there.
Corpora and findings (`fuzz/artifacts`) are not committed.

With AddressSanitizer, so a memory error stops the run where it happens. It is an
unstable compiler option; `RUSTC_BOOTSTRAP=1` lets the release toolchain (Rust
1.97.0, which ships the sanitizer's runtime for x86_64 Linux) accept it, so the code
fuzzed is compiled by the compiler that builds releases. CI runs every target this
way weekly (`.github/workflows/fuzz.yml`):

```sh
RUSTC_BOOTSTRAP=1 cargo fuzz run --sanitizer address config -- -max_total_time=900 -timeout=10
```

Without a sanitizer, faster, on any stable Rust:

```sh
cargo fuzz run -s none parse -- -max_total_time=900 -timeout=10
```

Most of what the fuzzers reach is safe Rust (and ring's own C and assembly, for
`jwt`). `config` and `journal` run the crash-test workflow, whose functions may run
as native code (docs/security-review/UNSAFE.md): fuzz those two with
AddressSanitizer.

A finding is a bug: reduce it with `cargo fuzz tmin`, add the reduced input as a
test where the code lives, fix it, and run the fuzzer again.
