#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
cd "$ROOT"
case "${1:-help}" in
    load)
        sha256sum -c SHA256SUMS
        docker load -i images.tar
        ;;
    init)
        if [[ ! -f .env ]]; then
            umask 077
            docker run --rm --network none --entrypoint python quickagent-customer-ppt-arm64:20261111 -c '
import secrets
for name in ["MYSQL_ROOT_PASSWORD","MYSQL_PASSWORD","REDIS_PASSWORD","JWT_SIGNING_KEY","CUSTOMER_ADMIN_PASSWORD"]:
    print(name+"="+secrets.token_hex(16))
print("COMPOSE_PROJECT_NAME=quickagent-ppt-customer\nDEEPSEEK_API_KEY=\nPPT_PORT=8899\nPPT_BIND=0.0.0.0\nQUICKAGENT_ARM64_EMULATED=0")
' > .env
            printf '已生成本机 .env；初始管理员密码保存在 CUSTOMER_ADMIN_PASSWORD。\n'
        fi
        mkdir -p state
        docker compose run --rm init
        printf '初始化完成。填写 .env 的 DEEPSEEK_API_KEY 后执行 ./deploy.sh up。\n'
        ;;
    up)
        [[ -f .env ]] || { printf '先执行 ./deploy.sh init\n' >&2; exit 1; }
        if ! awk -F= '$1=="DEEPSEEK_API_KEY" && length($2)>0 {found=1} END {exit !found}' .env; then
            printf '请先填写 .env 中非自用 DeepSeek 的 DEEPSEEK_API_KEY。\n' >&2; exit 1
        fi
        docker compose up -d app
        ;;
    status) docker compose ps ;;
    logs) docker compose logs --tail=100 -f app renderer ;;
    stop) docker compose stop ;;
    *) printf '用法: %s {load|init|up|status|logs|stop}\n' "$0" ;;
esac
