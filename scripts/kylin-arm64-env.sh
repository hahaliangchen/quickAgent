#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
TOOLS_IMAGE="${KYLIN_TOOLS_IMAGE:-quickagent-kylin-arm64-env:v1}"
BROWSER_IMAGE="${KYLIN_BROWSER_IMAGE:-quickagent-ppt-browser-arm64-env:v1}"
SERVER="${AI_PPT_SERVER_ROOT:-$ROOT/../ai-ppt-server}"
VIEW="${AI_PPT_VIEW_ROOT:-$ROOT/../ai-ppt-view}"
ENGINE="${PPTX_ENGINE_ROOT:-$HOME/project/pptx-editor-engine}"
CONTEXT="$ROOT/target/kylin-arm64/environment-context"
command="${1:-build}"
[[ $# == 0 ]] || shift
usage() {
    cat <<'EOF'
用法: bash scripts/kylin-arm64-env.sh [build|check|shell|browser-shell] [命令...]
  build          只构建并验证工具环境，不构建 QuickAgent/AI PPT 应用
  check          在无外网容器中验证工具链和 ARM64 Chromium
  shell          进入麒麟工具环境；可追加命令代替 bash
  browser-shell  进入 PPT 浏览器环境；可追加命令代替 bash
可覆盖 AI_PPT_SERVER_ROOT、AI_PPT_VIEW_ROOT、PPTX_ENGINE_ROOT、KYLIN_BASE_IMAGE。
独立安装开机 QEMU 注册：以 root 执行 bash scripts/setup-docker-arm64.sh --persist。
EOF
}
case "$command" in
    -h|--help) usage; exit 0 ;;
    build|check|shell|browser-shell) ;;
    *) usage >&2; exit 1 ;;
esac
bash "$ROOT/scripts/setup-docker-arm64.sh"
EMULATED=1
case "$(docker info --format '{{.Architecture}}')" in aarch64|arm64) EMULATED=0 ;; esac
if [[ "$command" == build ]]; then
    [[ $# == 0 ]] || { usage >&2; exit 1; }
    python3 "$ROOT/docker/kylin-arm64/fetch-tools.py" --context "$CONTEXT" --server "$SERVER" --view "$VIEW"
    host_args=()
    # Resolve the vendor repository on the host. A user may supply an internal
    # mirror's IP; the mapping belongs only to this build, never /etc/hosts.
    repo_ip="${KYLIN_REPO_IP:-}"
    if [[ -z "$repo_ip" ]]; then
        repo_ip="$(timeout 5 getent ahostsv4 update.cs2c.com.cn | awk 'NR == 1 {print $1}' || true)"
    fi
    if [[ -z "$repo_ip" ]]; then
        repo_ip="$(python3 - <<'PY'
import json, urllib.request
try:
    with urllib.request.urlopen('https://dns.google/resolve?name=update.cs2c.com.cn&type=A', timeout=15) as response:
        print(next(a['data'] for a in json.load(response).get('Answer', []) if a['type'] == 1))
except Exception:
    pass
PY
)"
    fi
    if [[ -n "$repo_ip" ]]; then
        python3 -c 'import ipaddress,sys; ipaddress.ip_address(sys.argv[1])' "$repo_ip"
        host_args+=(--add-host "update.cs2c.com.cn:$repo_ip")
    fi
    docker build --platform linux/arm64 --network "${KYLIN_BUILD_NETWORK:-host}" \
        --build-arg HTTP_PROXY --build-arg HTTPS_PROXY --build-arg ALL_PROXY --build-arg NO_PROXY \
        "${host_args[@]}" --build-arg "BASE_IMAGE=${KYLIN_BASE_IMAGE:-macrosan/kylin@sha256:525d7ef1663a5d9debcfeb041a8759608e890efc08ab70d9bed2804b309552ba}" \
        --build-arg "BUILD_JOBS=${KYLIN_BUILD_JOBS:-4}" \
        -t "$TOOLS_IMAGE" -f "$CONTEXT/Dockerfile" "$CONTEXT"
    docker build --platform linux/arm64 --build-arg "TOOLS_IMAGE=$TOOLS_IMAGE" \
        -t "$BROWSER_IMAGE" -f "$CONTEXT/browser.Dockerfile" "$CONTEXT"
    command=check
fi
for image in "$TOOLS_IMAGE" "$BROWSER_IMAGE"; do
    [[ "$(docker image inspect "$image" --format '{{.Architecture}}')" == arm64 ]] || {
        printf '不是 ARM64 环境镜像: %s\n' "$image" >&2; exit 1;
    }
done
if [[ "$command" == check ]]; then
    docker run --rm --platform linux/arm64 --network none "$TOOLS_IMAGE" bash /opt/environment/check.sh
    docker run --rm --platform linux/arm64 --network none --shm-size=1g \
        -e "QUICKAGENT_ARM64_EMULATED=$EMULATED" "$BROWSER_IMAGE" node /opt/environment/check-browser.cjs
    exit 0
fi
for path in "$SERVER" "$VIEW" "$ENGINE"; do
    [[ -d "$path" ]] || { printf '项目目录不存在: %s\n' "$path" >&2; exit 1; }
done
# ARM-specific volumes hide the host's x86/Windows node_modules and Cargo output.
mounts=(
    -v "$ROOT:/work/agent" -v "$(realpath "$SERVER"):/work/ai-ppt-server"
    -v "$(realpath "$VIEW"):/work/ai-ppt-view" -v "$(realpath "$ENGINE"):/work/pptx-engine"
    -v quickagent-arm64-node-agent:/work/agent/frontend/node_modules
    -v quickagent-arm64-node-view:/work/ai-ppt-view/node_modules
    -v quickagent-arm64-node-engine:/work/pptx-engine/node_modules
    -v quickagent-arm64-cargo-target:/work/agent/target
    -v quickagent-arm64-wasm-target:/work/pptx-engine/rust-engine/target
)
tty_args=()
[[ ! -t 0 || ! -t 1 ]] || tty_args=(-it)
[[ $# -gt 0 ]] || set -- bash
image="$TOOLS_IMAGE"
[[ "$command" != browser-shell ]] || image="$BROWSER_IMAGE"
exec docker run --rm --init --platform linux/arm64 "${tty_args[@]}" --shm-size=1g \
    "${mounts[@]}" -e PPTX_ENGINE_DIR=/work/pptx-engine \
    -e "PPT_CUSTOMER_EDITION=${PPT_CUSTOMER_EDITION:-1}" \
    -e "QUICKAGENT_ARM64_EMULATED=$EMULATED" -w /work/agent "$image" "$@"
