#!/usr/bin/env bash
set -Eeuo pipefail
browser="$(node -e 'process.stdout.write(require(process.env.PPT_PLAYWRIGHT_MODULE).chromium.executablePath())')"
[[ -x "$browser" ]] || { printf 'ARM64 Chromium 不存在: %s\n' "$browser" >&2; exit 1; }
# The host runner detects Docker's real architecture: QEMU can synthesize an
# ARM /proc/cpuinfo, so it cannot reliably be detected inside this container.
if [[ "${QUICKAGENT_ARM64_EMULATED:-}" == 1 ]]; then
    exec "$browser" --disable-gpu --disable-software-rasterizer --no-zygote --single-process "$@"
fi
exec "$browser" "$@"
