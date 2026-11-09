# Building, testing and reproducing the evidence

How to build the reviewed commit and rerun everything this package cites. Commands
are run from the repository root.

## 1. Requirements

- Rust 1.97.0 (what CI uses; the minimum is 1.89): `rustup toolchain install 1.97.0`
- `git`, a C compiler (`cc`), Python 3, Node.js 22
- Optional: PostgreSQL server binaries for `tests/postgres_test.rs` (from
  `STAAL_POSTGRES_BIN`, the newest `/usr/lib/postgresql/*/bin`, or `pg_config
  --bindir`; without one those tests fail unless `STAAL_SKIP_POSTGRES=1`)
- Linux or macOS. Some tests are Linux only (SQLite, executables), as the features
  are (LIMITATIONS.md section 5).

## 2. Building

```sh
git checkout <the commit in README.md>
cargo build --release --locked                 # the release build
cargo build --locked --features failpoints     # the test build: crash injection, and
                                               # the only build that serves MCP over HTTP
cargo build --locked --no-default-features     # without the native engine
```

`failpoints` must never be enabled in a release build: it adds crash injection
(`STAALC_FAILPOINT`), test-only timing settings, and the MCP HTTP transport
(`server.http`), which a release build refuses until this review is done. Test
`a_release_build_refuses_http_until_it_has_authentication` checks the release build
refuses it.

## 3. Tests

What CI runs, on Linux x86_64 and arm64 and macOS on Apple Silicon and Intel
(`.github/workflows/ci.yml`):

```sh
cargo test --features failpoints --no-fail-fast       # everything, with crash injection
cargo test --no-default-features --no-fail-fast       # without native code
cargo clippy --all-targets --features failpoints -- -D warnings
```

The MCP SDK conformance tests (official Python and TypeScript clients over stdio and
over HTTPS with OAuth) need the SDKs installed:

```sh
python3 -m venv /path/to/venv
/path/to/venv/bin/pip install --uploaded-prior-to=P14D -r tests/mcp_sdk/requirements.txt
(cd tests/mcp_sdk && npm ci)
STAAL_MCP_SDK=1 STAAL_MCP_PYTHON=/path/to/venv/bin/python \
  cargo test --features failpoints --test mcp_sdk_test
```

Running one area: `cargo test --features failpoints --test mcp_http_test`; the test
files are listed in [ASSURANCE.md](ASSURANCE.md).

## 4. Supply chain

```sh
cargo install --locked cargo-deny --version 0.20.2
cargo install --locked cargo-audit --version 0.22.2
cargo deny check && cargo audit
cargo install --locked cargo-cyclonedx --version 0.5.9
cargo cyclonedx --format json --all-features --spec-version 1.5 --target all
```

## 5. The `unsafe` inventory

```sh
python3 scripts/unsafe_inventory.py     # the table in UNSAFE.md section 4
```

## 6. Fuzzing

```sh
cargo install --locked cargo-fuzz --version 0.13.1
scripts/fuzz_seed.sh
RUSTC_BOOTSTRAP=1 cargo fuzz run --sanitizer address engines -- -max_total_time=900 -timeout=30
cargo fuzz run -s none parse -- -max_total_time=900 -timeout=10
```

Targets: `parse`, `format`, `journal`, `config`, `manifest`, `jwt`, `engines`
([fuzz/README.md](../../fuzz/README.md) says what each checks, and why
`RUSTC_BOOTSTRAP=1` is used for AddressSanitizer).

## 7. The JWT test corpus

`tests/fixtures/jwt/` was made by `scripts/jwt_fixtures.py` with PyJWT, an
implementation independent of Staal's. It is committed so the tests do not need
Python; to remake it (new keys, so the files change):

```sh
python3 -m venv /path/to/venv && /path/to/venv/bin/pip install pyjwt cryptography
/path/to/venv/bin/python scripts/jwt_fixtures.py
```
