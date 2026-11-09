# Supply chain

What Staal depends on, how each dependency is chosen and checked, and how a release
gets from the source to a user.

## 1. Rust dependencies

One crate, `staalc`. `Cargo.lock` is committed and fixes every version; the
Node.js addon (`node/Cargo.lock`) and the fuzz targets (`fuzz/Cargo.lock`) have their
own. `Cargo.lock` lists 238 packages (every platform, development dependencies
included). The SBOM in [sbom/](sbom/) lists those built for the release platforms.

Direct dependencies of the library and binary, and what each is for:

| Crate | Version | Used for | Security-relevant |
| --- | --- | --- | --- |
| `rustls` (ring provider) | =0.23.46 | every TLS connection, client and server | yes: TLS |
| `ring` | =0.17.14 | JWT signatures, HMAC-SHA-256 (principal scope), rustls's crypto | yes: crypto |
| `webpki-root-certs` | 1.0.11 | Mozilla's root certificates | yes: trust anchors |
| `x509-cert` | =0.2.5 | reading a listening certificate's validity | yes |
| `ureq` (rustls) | 3.4.2 | outgoing HTTP: `net.http`, model providers, key discovery | yes: parser |
| `hyper`, `hyper-util`, `http-body`, `http-body-util` | =1.12.0, =0.1.21, =1.1.0, =0.1.5 | the MCP server over HTTP | yes: request framing |
| `tokio`, `tokio-rustls` | =1.54.1, =0.26.6 | the HTTP server's runtime and TLS | |
| `postgres`, `tokio-postgres-rustls` | =0.19.14, =0.14.0 | `db.postgres` | yes: protocol |
| `rusqlite` (bundled SQLite) | 0.41.0 | `db.sqlite` | yes: SQL engine (C) |
| `serde_json` | 1.0.154 | every JSON document | yes: parser |
| `sha2` | 0.11.1 | journals, packages, executables | yes: hashing |
| `base64`, `url`, `bytes`, `libc`, `getrandom` | 0.23.1, 2.5.8, 1.12.1, 0.2.191, 0.4.3 | encoding, URLs, buffers, system calls, random numbers | |
| `cranelift-*` (6 crates, optional, `native` feature) | =0.136.2 | the native engine | yes: code generation |

C code built from source: SQLite (through `rusqlite`'s `bundled` feature) and ring's
assembly and C. Nothing else links C at build time; `cap ffi` loads C libraries the
operator names at run time (THREAT_MODEL.md T55).

## 2. Policy

- **Age.** A dependency version is chosen only when it has been published for at
  least two weeks (CONTRIBUTING.md), so a malicious release has time to be found
  and yanked before it is used. This includes security fixes: the advisories in
  section 4 were fixed with versions at least two weeks old.
- **Exact versions** for the crates that handle TLS, crypto, HTTP framing and code
  generation (`=` in `Cargo.toml`); `Cargo.lock` fixes every other one.
- **Sources.** crates.io only; no git dependency and no other registry (`deny.toml`
  `[sources]`).
- **Licences.** Permissive only, listed in `deny.toml`; NCSA allowed for
  `libfuzzer-sys` alone, which only the fuzz targets use.
- **Features.** Default features off where they would add code not used
  (`rustls`, `ureq`, `hyper`, `tokio`, `x509-cert`).

## 3. Checks

`.github/workflows/supply-chain.yml`, on every push and pull request and weekly
(Wednesdays 05:41 UTC), for `staalc`, `node/` and `fuzz/`:

- `cargo deny check` (cargo-deny 0.20.2) against `deny.toml`: RustSec advisories
  (vulnerabilities, unsound crates) and yanked versions refused; an unmaintained-crate
  advisory refused for a crate the workspace depends on directly; licences; sources; wildcard requirements refused.
  Checked for the four release platforms.
- `cargo audit` (cargo-audit 0.22.2) against the RustSec database.
- `npm audit` for the Node.js package's lock.

An exception needs an entry in `deny.toml` naming the advisory and why it does not
apply. There are none.

## 4. Findings of the first run

The first run of these checks, during preparation for this review, found three
advisories, all in the TLS trust chain, all fixed in commit `cc7ab1e`:

| Advisory | Crate | Problem | Fixed by |
| --- | --- | --- | --- |
| RUSTSEC-2026-0382 | `rustls-webpki` | could accept distrusted emSign intermediates | 0.103.15 → 0.103.16 |
| RUSTSEC-2026-0381 | `webpki-roots` | bundled the distrusted emSign ECC Root CA - G3 | 1.0.9 → 1.0.11 |
| RUSTSEC-2026-0383 | `webpki-root-certs` | the same root | 1.0.9 → 1.0.11 |

They affected certificate checking in `net.http`, `net.tls`, PostgreSQL and the HTTP
transport's key fetches. Before this, dependencies were not checked against advisories
automatically ([SELF_ASSESSMENT.md](SELF_ASSESSMENT.md) F1).

## 5. Other ecosystems

- **Python package** (`python/`): no runtime dependencies. Optional extras for the
  framework adapters (`openai-agents`, `langgraph`, `claude-agent-sdk`, `google-adk`)
  name lower bounds only; the user's resolver chooses the versions.
- **Node.js package** (`node/`): no runtime dependencies.
- **Test and build tools**: the SDK conformance and framework tests install from
  requirement files naming the top-level packages at exact versions, with
  `pip --uploaded-prior-to=P14D` (nothing published in the last 14 days, which also
  bounds the versions of their dependencies) and `npm ci` from committed lock files.
  CI tools are installed at exact versions with `cargo install --locked`.
  The Linux release builds use `ziglang==0.13.0` from PyPI and `cargo-zigbuild`
  0.19.8.

## 6. CI and release

- **Actions** are pinned to full commit SHAs, with the version in a comment
  (`.github/workflows/`); a moved tag cannot change what runs.
- **Permissions**: `contents: read` by default; `id-token: write` only on the
  publishing jobs (`publish.yml`) and the documentation site (`pages.yml`). No
  workflow uses `pull_request_target`; event data reaches shell steps through
  environment variables, not interpolation into scripts.
- **Builds**: release builds and the wheel build use `cargo build --locked`, so a
  build cannot silently resolve a different version than `Cargo.lock` names.
- **Release** (`release.yml`, on a tag): checks that the tag equals `Cargo.toml`'s
  version; builds and tests on each platform; makes the archive with a `.sha256`
  file, the wheels and the npm package; creates a draft GitHub release, which the
  maintainer publishes.
- **Publishing** (`publish.yml`): PyPI and npm through trusted publishing (OIDC),
  so no publishing token is stored; npm packages carry provenance. The first npm
  version was published by hand, as npm requires before a trusted publisher can be
  set (RELEASING.md).
- **Build provenance**: the release job attests every archive, wheel and npm package
  with `actions/attest` (v4.2.2, pinned): an in-toto SLSA provenance statement naming
  the file's SHA-256, signed with a short-lived Sigstore certificate issued for this
  repository's release workflow, its run and its tag, and stored by GitHub. Releases
  up to 0.26.0 predate it; the first release to carry it is 0.27.0, where it is
  first exercised (docs/RELEASING.md).
- **Installing**: `install.sh` downloads an archive and its `.sha256` from the GitHub
  release over HTTPS and checks the hash before unpacking. Then, when the GitHub CLI
  is installed and signed in, it runs `gh attestation verify` requiring the
  repository, the release workflow, the tag `v<version>` and a GitHub-hosted runner,
  and refuses an archive of a release after 0.26.0 that fails. Without `gh` it says
  the provenance was not checked and installs on the hash alone;
  `STAAL_REQUIRE_PROVENANCE=1` refuses instead. It installs under the user's home.
  Tested against a stand-in `gh` (`install_test.rs`).

What remains: a user without the GitHub CLI gets the hash check only, which detects
corruption but not a release replaced by someone with write access to the
repository ([SELF_ASSESSMENT.md](SELF_ASSESSMENT.md) F6).

## 7. SBOM

[sbom/](sbom/): CycloneDX 1.5, one SBOM per release platform, generated at the
reviewed commit with cargo-cyclonedx 0.5.9 for the release build (default features,
no development dependencies):

    for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-apple-darwin aarch64-apple-darwin; do
      cargo cyclonedx --format json --spec-version 1.5 --target $t --target-in-filename
    done

| Platform | Components |
| --- | ---: |
| `x86_64-unknown-linux-gnu` | 177 |
| `aarch64-unknown-linux-gnu` | 177 |
| `x86_64-apple-darwin` | 180 |
| `aarch64-apple-darwin` | 180 |

Each component has its version, licence and crates.io package URL (purl).
