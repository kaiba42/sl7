#!/usr/bin/env bash
set -euo pipefail
cargo fmt --all -- --check
cargo check --locked --offline --no-default-features
cargo test --locked --offline --features fuse-check
cargo clippy --locked --offline --all-targets --features fuse-check -- -D warnings
