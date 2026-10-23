#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
context="${SL7_DOCKER_CONTEXT:-orbstack}"
image="sl7-mount-test:local"
docker --context "$context" build -f tests/docker/Dockerfile -t "$image" .
# Mount capability exists only in this disposable container's namespace.
# No host directories or Docker socket are mounted into the container.
docker --context "$context" run --rm --device /dev/fuse --cap-add SYS_ADMIN \
    --security-opt apparmor=unconfined "$image"
