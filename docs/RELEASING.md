# Releasing staalc

A release is a version tag. Pushing one runs `.github/workflows/release.yml`, which
on Linux (in a manylinux_2_28 container, so for glibc 2.28 and later), Apple Silicon
and Intel macOS runs the tests, packages the release archive (`scripts/package.sh`)
and the Python wheel (`scripts/wheel.sh`), installs the wheel into a clean
environment and uses it with no Rust (`scripts/wheel_test.sh`), and makes a draft
GitHub release holding them all, with the changelog's notes for the version.

1. Move the changelog's `## Unreleased` entries under `## X.Y.Z — YYYY-MM-DD`.
2. Set version X.Y.Z in `Cargo.toml`, `python/pyproject.toml`,
   `editors/vscode/package.json`, `node/package.json` and `node/Cargo.toml`.
   `cargo test --test release_test` fails until all of them and the changelog agree.
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
executables from `staalc build` on macOS (it makes them on Linux only).

## Linux builds without CI

While CI cannot run, Linux x86_64 binaries for glibc 2.28 and later (the same
baseline as the manylinux_2_28 container) are built on any Linux machine with
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), which links against
an older glibc through Zig:

    pip install ziglang==0.13.0
    cargo install --locked cargo-zigbuild --version 0.19.8
    cargo zigbuild --release --locked --target x86_64-unknown-linux-gnu.2.28 --target-dir target/zig

    # The wheel, from those binaries, tagged manylinux_2_28, then tested installed.
    STAAL_RELEASE_DIR=target/zig/x86_64-unknown-linux-gnu/release AUDITWHEEL_PLAT=manylinux_2_28_x86_64 \
        PYTHON=<python with build, auditwheel, patchelf> scripts/wheel.sh dist
    scripts/wheel_test.sh dist/staal-X.Y.Z-py3-none-manylinux_2_28_x86_64.whl

    # The npm package, its addon built the same way.
    STAAL_ZIGBUILD=1 scripts/node_package.sh dist

`objdump -T <binary> | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1` shows the newest
glibc a binary needs: it must be at most 2.28. These have been tested on the machine that
built them, which has a newer glibc, not yet on a glibc 2.28 system.

## Publishing to PyPI and npm

Both are public and permanent: a version, once uploaded, can be withdrawn but never
replaced.

Publishing the draft release runs `.github/workflows/publish.yml`, which uploads the
release's own wheels to PyPI and its npm package to npm: the files CI built and
tested, nothing rebuilt. Both use trusted publishing, so no token is stored anywhere. It needs, once:

- On pypi.org: Account settings, Publishing, add a pending publisher with project
  `staal`, owner `laurells`, repository `staal`, workflow `publish.yml`, environment
  `pypi`.
- On npm: the package must exist before a trusted publisher can be set, so its first
  version is published by hand (`npm login`, then `npm publish staal-node-X.Y.Z.tgz
  --access public`, with the file from the GitHub release). Then on npmjs.com: the
  `@staal/node` package (in the npm organisation `staal`), Settings, Trusted Publisher, GitHub Actions, with owner
  `laurells`, repository `staal`, workflow `publish.yml`, environment `npm`.
- Environments named `pypi` and `npm` in the repository (Settings, Environments).

A version already on PyPI or npm is skipped, so publishing a release whose npm
package was uploaded by hand still uploads its wheels.

It can also be run by hand for a tag (Actions, publish, Run workflow).

Without CI, the files built as in "Linux builds without CI" are uploaded from the
machine that built them. Only Linux x86_64 files exist that way, so `pip install staal`
and `npm install @staal/node` then fail on other platforms with no matching file.

    # PyPI: an API token from pypi.org (Account settings, API tokens); user __token__.
    python3 -m pip install twine
    python3 -m twine check dist/*.whl
    python3 -m twine upload dist/*.whl

    # npm: an npmjs.com account, logged in once with `npm login`.
    npm publish dist/staal-node-X.Y.Z.tgz

Upload only the wheel, never an sdist: the sdist holds no binaries and cannot build
them.
