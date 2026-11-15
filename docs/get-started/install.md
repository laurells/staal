# Install

`staalc` is a single program: the compiler, the runtime and the tools (`check`,
`run`, `test`, `fmt`, `debug`, `lsp` and the rest; see the
[command-line reference](../reference/cli.md)).

## Supported platforms

| Platform | Minimum |
| --- | --- |
| Linux, x86_64 and arm64 | glibc 2.28 |
| macOS, Apple Silicon | macOS 11 |
| macOS, Intel | macOS 10.12 |

Windows and Linux with musl are not supported. `staalc build` (standalone
executables) and the `db.sqlite` capabilities work on Linux only.

## Install `staalc`

```sh
curl -fsSL https://raw.githubusercontent.com/laurells/staal/main/install.sh | sh
```

The script downloads the latest release's archive for the machine, refuses it unless
its SHA-256 matches the one published with the release, unpacks it into
`~/.staal/<version>` and links `staalc` into `~/.local/bin`. It prints a command to
add that folder to `PATH` if it is not there. It needs `curl`, `tar`, and `sha256sum`
or `shasum`, and it does not need root.

If the GitHub CLI (`gh`) is installed and signed in, the script also verifies the
archive's build provenance attestation (made by the repository's release workflow
from the release's tag) and refuses an archive that fails it. Without `gh`, only the
SHA-256 is checked, and the script says so.

| Setting | Effect |
| --- | --- |
| `STAAL_VERSION=0.29.0` | install this version instead of the latest |
| `STAAL_BIN_DIR=DIR` | link `staalc` into `DIR` instead of `~/.local/bin` |
| `STAAL_HOME=DIR` | unpack releases into `DIR` instead of `~/.staal` |
| `STAAL_REQUIRE_PROVENANCE=1` | refuse to install unless the provenance is verified |
| `STAAL_DOWNLOAD_BASE=URL` | download from a mirror laid out as `URL/v<version>/<archive>` |
| `STAAL_GH=PATH` | the GitHub CLI to verify with |

```sh
curl -fsSL https://raw.githubusercontent.com/laurells/staal/main/install.sh | STAAL_VERSION=0.29.0 sh
```

## Install from Python

```sh
pip install staal
```

The wheel contains `staalc` and the `staal` Python package, which runs programs
through Staal's C interface. It needs Python 3.8 or later. See
[Calling Staal from an app](../guides/apps.md).

## Install from Node.js

```sh
npm install @staal/node
```

`@staal/node` is a library for Node.js 18 or later. It contains the runtime, but it
does not install the `staalc` command; install `staalc` separately to check, test and
format programs.

## Build from source

Requirements: Rust 1.89 or later, `git` (to fetch packages), and a C compiler (`cc`)
for `staalc build`.

```sh
git clone https://github.com/laurells/staal.git
cd staal
cargo install --path . --locked
```

This installs `staalc` into `~/.cargo/bin`. To build without the native code engine
(interpreter only; `staalc build` and `--engine native` are then refused):

```sh
cargo install --path . --locked --no-default-features
```

## Verify

```sh
staalc --version
```

prints `staalc` and the version, for example `staalc 0.29.0`.

## Editor support

`staalc lsp` is a language server on standard input and output: it reports the
errors `staalc check` reports, as you type, and provides completion and formatting.
Any editor with a language server client can use it.

A VS Code extension is in [editors/vscode](../../editors/vscode/README.md). It is not
published on the Marketplace; its README shows how to build and install it.
