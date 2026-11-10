# Journals from releases

Each `staalc-<version>/` holds what that release's own binary wrote in one
uninterrupted run of each scenario of `tests/journal_compat_test.rs`: the program and
config it ran, its `journal`, its `world.jsonl` and its `stdout`. The test derives every
crash state from them and resumes each with the `staalc` being built (AGENT_MANIFEST.md
section 16, R5). Never edit these files: they are a record of what a release wrote.

| Set | Journal version | Binary | SHA-256 of the archive |
| --- | --- | --- | --- |
| `staalc-0.23.0` | 2 | `staalc-0.23.0-x86_64-unknown-linux-gnu.tar.gz` from the GitHub release v0.23.0 | `6776c6f6cb9751d8e631aaa80d8eec8532e064c0027a48fe9e39d8b3f4c10cc6` |
| `staalc-0.27.0` | 2 | `staalc-0.27.0-x86_64-unknown-linux-gnu.tar.gz` from the GitHub release v0.27.0 | `5cd2d9465cadcee12d334809e6efc6d285e534eb3cb10da5d304294d9d08dc1f` |

Each archive's SHA-256 was checked against the `.sha256` file published with it. That
file comes from the same release, so it shows the archive arrived whole, not who built
it. 0.27.0 also has a build provenance attestation; it could not be checked where these
sets were recorded, because Sigstore was unreachable from there. 0.23.0 has no
attestation. No release before 0.23.0 published a Linux binary, so no set holds a
journal of version 1.

`staalc` reads journal version 3 only. So both sets show that a journal of an older
version is refused without anything being run or changed. The first release that writes
version 3 is recorded as described in `tests/journal_compat_test.rs` (`UNRELEASED`), and
from then on every one of its crash states must resume to exactly that release's output
and world.

To record a release (before running the generator, check the archive's SHA-256 and,
where the release has one, its attestation with
`gh attestation verify <archive> --repo laurells/staal`):

    STAALC_FIXTURE_BIN=/path/to/staalc cargo test --test journal_compat_test -- --ignored generate_fixtures
