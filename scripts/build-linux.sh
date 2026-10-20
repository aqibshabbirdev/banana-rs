#!/usr/bin/env bash
# Builds the Linux binary inside Docker (Ubuntu 24.04), so it can be made from a Mac.
# Output: target/linux/release/banana-rs for the host's CPU (arm64 on Apple Silicon).
# Pass --platform linux/amd64 via DOCKER_PLATFORM for an x86_64 build (slower, emulated).
set -euo pipefail
cd "$(dirname "$0")/.."

PLATFORM_ARGS=()
if [ -n "${DOCKER_PLATFORM:-}" ]; then
  PLATFORM_ARGS=(--platform "$DOCKER_PLATFORM")
fi

docker build "${PLATFORM_ARGS[@]}" -q -t banana-linux -f scripts/linux.Dockerfile scripts >/dev/null
docker run --rm "${PLATFORM_ARGS[@]}" \
  -v "$PWD":/src \
  -v banana-cargo-registry:/root/.cargo/registry \
  -e CARGO_TARGET_DIR=/src/target/linux \
  banana-linux cargo build --release "$@"

scripts/fetch-models.sh target/linux/release/models >/dev/null
echo "Built target/linux/release/banana-rs (People & pets, Documents and search models in target/linux/release/models)"
