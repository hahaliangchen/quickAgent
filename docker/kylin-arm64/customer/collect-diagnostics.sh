#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
cd "$ROOT"
if docker compose version >/dev/null 2>&1; then
    COMPOSE=(docker compose)
else
    COMPOSE=(docker-compose)
fi
mkdir -p logs
STAMP="$(date +%Y%m%d-%H%M%S)"
NAME="customer-diagnostics-${STAMP}.tar.gz"
# These scripts are mounted from the deployment folder, so the same command
# also works against an older installed image without changing that image.
"${COMPOSE[@]}" run --rm --no-deps -T --entrypoint python \
    -v "$ROOT/diagnostics.py:/tmp/customer-support/diagnostics.py:ro" \
    -v "$ROOT/customer_logging.py:/tmp/customer-support/customer_logging.py:ro" \
    -v "$ROOT/logs:/logs" \
    app /tmp/customer-support/diagnostics.py --state /state --logs /logs --output "/logs/$NAME"
printf '诊断包已生成：%s/logs/%s\n' "$ROOT" "$NAME"
