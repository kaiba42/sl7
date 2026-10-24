#!/usr/bin/env bash
set -euo pipefail
experiment="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$experiment" rev-parse --show-toplevel)"
work="$(mktemp -d "${TMPDIR:-/tmp}/sl7-chunker-buffer.XXXXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir "$work/src"
cp "$experiment/Cargo.toml" "$experiment/Cargo.lock" "$work/"
cp "$experiment/src/main.rs" "$work/src/"
git -C "$repo" show "$(cat "$experiment/base-commit"):src/chunk.rs" > "$work/src/original.rs"
cd "$work"
cargo build --locked --release
python3 "$experiment/run.py"
