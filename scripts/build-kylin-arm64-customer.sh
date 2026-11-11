#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
STAMP="$(date +%Y%m%d-%H%M%S)"
WORK="${CUSTOMER_BUILD_WORK:-$ROOT/target/kylin-arm64/customer-build-$STAMP}"
OUTPUT="${CUSTOMER_RELEASE_DIR:-$ROOT/target/kylin-arm64/releases/quickagent-ai-ppt-kylin-arm64-$STAMP}"
RECIPE="$ROOT/docker/kylin-arm64/customer"
bash scripts/setup-docker-arm64.sh
rustup toolchain install 1.98.1 --profile minimal
rustup target add --toolchain 1.98.1 aarch64-unknown-linux-gnu wasm32-unknown-unknown
if [[ "${1:-}" == --resume ]]; then
    python3 "$RECIPE/build.py" --work "$WORK" --resume
else
    python3 "$RECIPE/build.py" --work "$WORK"
fi
python3 "$RECIPE/assemble.py" --work "$WORK" --output "$OUTPUT"
python3 "$RECIPE/smoke.py" --work "$WORK/smoke" --output "$OUTPUT"
python3 "$RECIPE/finalize.py" --output "$OUTPUT"
