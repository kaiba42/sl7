#!/usr/bin/env bash
set -euo pipefail
experiment="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$experiment" rev-parse --show-toplevel)"
base="$(cat "$experiment/base-commit")"
work="$(mktemp -d "${TMPDIR:-/tmp}/sl7-import-pipeline.XXXXXXXX")"
context="${SL7_DOCKER_CONTEXT:-orbstack}"
image="sl7-import-pipeline-probe:run-$$"
cleanup() {
    docker --context "$context" image rm "$image" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
mkdir "$work/context"
git -C "$repo" archive "$base" | tar xf - -C "$work/context"
cp -R "$work/context/src" "$work/context/baseline-src"
git -C "$work/context" apply "$experiment/changes.patch"
printf '\n!baseline-src/\n!baseline-src/**\n' >> "$work/context/.dockerignore"
cp "$experiment/Dockerfile" "$work/context/Dockerfile.probe"
cp "$experiment/probe.py" "$work/context/scripts/pipeline-probe.py"
docker --context "$context" build -f "$work/context/Dockerfile.probe" -t "$image" "$work/context"
docker --context "$context" run --rm --cpus 4 --memory 4g "$image"
