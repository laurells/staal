# Releasing staalc

A release is a version tag. Pushing one runs `.github/workflows/release.yml`, which
on Linux (in a manylinux_2_28 container, so for glibc 2.28 and later), Apple Silicon
and Intel macOS runs the tests, packages the release archive (`scripts/package.sh`)
and the Python wheel (`scripts/wheel.sh`), installs the wheel into a clean
environment and uses it with no Rust (`scripts/wheel_test.sh`), and makes a draft
GitHub release holding them all, with the changelog's notes for the version.

1. Move the changelog's `## Unreleased` entries under `## X.Y.Z — YYYY-MM-DD`.
2. Set version X.Y.Z in `Cargo.toml`, `python/pyproject.toml` and
   `editors/vscode/package.json`. `cargo test --test release_test` fails until all
   three and the changelog agree.
3. Commit, then tag and push: `git tag -a vX.Y.Z -m "staalc X.Y.Z"` and
   `git push origin vX.Y.Z`. The workflow refuses a tag that is not `v` and the
   version in `Cargo.toml`.
4. Check the draft release's files, then publish it. `install.sh` installs from the
   latest published release, so a release is what users get once it is published.

Each archive is `staalc-X.Y.Z-<target>.tar.gz` with a `.sha256` beside it: `staalc`,
`libstaalc` to load from other languages, its static form (which `staalc build`
links), `include/staal.h`, the README and the changelog, and `LICENSE`, `NOTICE`
and `LICENSE-THIRD-PARTY.txt`: the licences of every crate the binaries contain,
written by `scripts/third_party_licenses.py` from `cargo metadata`. The wheel holds
the same `staalc`, libraries and licence files; `pip install` of it gives the `staalc` command and the
`staal` Python package, with no Rust needed.

Not yet in a release: Windows, other Linux architectures, WebAssembly, and
executables from `staalc build` on macOS (it makes them on Linux only). Publishing
the wheel to PyPI is a separate decision: a PyPI project is public.
