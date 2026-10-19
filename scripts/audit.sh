#!/bin/sh
# Fails if any dependency in Cargo.lock has a known security advisory (RustSec).
# Needs network access for the advisory database. Install once with:
#   cargo install cargo-audit --locked
set -e
cd "$(dirname "$0")/.."
cargo audit --deny warnings
