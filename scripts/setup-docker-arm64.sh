#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
IMAGE='tonistiigi/binfmt@sha256:400a4873b838d1b89194d982c45e5fb3cda4593fbfd7e08a02e76b03b21166f0'
PROBE='arm64v8/alpine:3.22'
case "${1:-}" in
    '') persist=0 ;;
    --persist) persist=1 ;;
    -h|--help) printf '用法: %s [--persist]\n检查并修复 Docker ARM64 执行；--persist 以 root 安装开机注册服务。\n' "$0"; exit 0 ;;
    *) printf '未知参数: %s\n' "$1" >&2; exit 1 ;;
esac
[[ $# -le 1 ]] || exit 1
[[ "$persist" == 0 || "$EUID" == 0 ]] || { printf '%s\n' '--persist 需要以 root 执行。' >&2; exit 1; }
docker info >/dev/null
docker image inspect "$PROBE" >/dev/null 2>&1 || docker pull --platform linux/arm64 "$PROBE"
probe() {
    local arch
    arch="$(docker run --rm --platform linux/arm64 --network none --entrypoint /bin/uname "$PROBE" -m 2>/dev/null)" || return 1
    [[ "$arch" == aarch64 || "$arch" == arm64 ]]
}
if ! probe; then
    case "$(docker info --format '{{.Architecture}}')" in
        aarch64|arm64) printf '原生 ARM64 Docker 探针失败，请检查镜像。\n' >&2; exit 1 ;;
    esac
    printf '注册 QEMU ARM64 binfmt_misc（Docker 特权容器）。\n'
    docker run --privileged --rm "$IMAGE" --install arm64
    probe || { printf 'ARM64 执行仍失败。\n' >&2; exit 1; }
fi
printf 'ARM64 容器执行验证通过。\n'
if [[ "$persist" == 1 ]]; then
    docker image inspect "$IMAGE" >/dev/null 2>&1 || docker pull "$IMAGE"
    install -m 0644 "$ROOT/docker/kylin-arm64/quickagent-arm64-binfmt.service" /etc/systemd/system/quickagent-arm64-binfmt.service
    systemctl daemon-reload
    systemctl enable --now quickagent-arm64-binfmt.service
fi
