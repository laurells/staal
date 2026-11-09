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

Install once: `cargo install cargo-fuzz`. A nightly Rust is not needed: with
`-s none` (no AddressSanitizer) the fuzzers build on stable. Most of what the
fuzzers reach is safe Rust (and ring's own C and assembly, for `jwt`). `config` and
`journal` run the crash-test workflow, whose functions may run as native code
(docs/security-review/UNSAFE.md); without AddressSanitizer a memory error there shows
only if it crashes or fails an assertion.

```sh
cargo fuzz run -s none parse fuzz/corpus/parse -- -max_total_time=600 -timeout=10 -max_len=8192
```

Seed corpora: copy the repository's programs into `fuzz/corpus/parse` and
`fuzz/corpus/format` (`find tests examples packages docs -name '*.staal'`), run
configs into `fuzz/corpus/config`, journals from a test run (without their first
line) into `fuzz/corpus/journal`, and `staal.json` and `staal.lock` files into
`fuzz/corpus/manifest`. Corpora and findings (`fuzz/artifacts`) are not committed.

A finding is a bug: reduce it with `cargo fuzz tmin`, add the reduced input as a
test where the code lives, fix it, and run the fuzzer again.
