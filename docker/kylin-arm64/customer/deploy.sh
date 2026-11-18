#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
cd "$ROOT"
ensure_models_file() {
    if [[ -d models.yaml ]]; then
        printf 'models.yaml 不能是目录，请先移除后重新创建为文件\n' >&2
        exit 1
    fi
    if [[ ! -f models.yaml ]]; then
        printf 'providers: []\n' > models.yaml
        chmod 600 models.yaml
    fi
}
case "${1:-help}" in
    load)
        sha256sum -c SHA256SUMS
        docker load -i images.tar
        ;;
    init)
        if [[ ! -f .env ]]; then
            umask 077
            docker run --rm --network none --entrypoint python quickagent-customer-ppt-arm64:20261118 -c '
import secrets
for name in ["MYSQL_ROOT_PASSWORD","MYSQL_PASSWORD","REDIS_PASSWORD","JWT_SIGNING_KEY","CUSTOMER_ADMIN_PASSWORD"]:
    print(name+"="+secrets.token_hex(16))
print("COMPOSE_PROJECT_NAME=quickagent-ppt-customer\nPPT_PORT=8899\nPPT_BIND=0.0.0.0\nQUICKAGENT_PORT=3001\nQUICKAGENT_BIND=0.0.0.0\nQUICKAGENT_ARM64_EMULATED=0\nONLYOFFICE_SERVER_ADDRESS=\nONLYOFFICE_DOC_SERVER_HOST=\nONLYOFFICE_JWT_SECRET=\nONLYOFFICE_JWT_ENABLED=true\nONLYOFFICE_PLUGIN_BASE_URL=\nONLYOFFICE_WORKBENCH_API_BASE_URL=")
' > .env
            printf '已生成本机 .env；初始管理员密码保存在 CUSTOMER_ADMIN_PASSWORD。\n'
        fi
        mkdir -p state logs
        ensure_models_file
        docker compose run --rm init
        printf '初始化完成。执行 ./deploy.sh up；可在 models.yaml 或设置页配置自部署模型，免鉴权模型的 API 密钥可留空。\n'
        ;;
    up)
        [[ -f .env ]] || { printf '先执行 ./deploy.sh init\n' >&2; exit 1; }
        mkdir -p logs
        ensure_models_file
        docker compose up -d --wait --wait-timeout 480 app
        ;;
    status) docker compose ps ;;
    logs) docker compose logs --tail=100 -f app renderer ;;
    diagnostics) exec bash "$ROOT/collect-diagnostics.sh" ;;
    stop) docker compose stop ;;
    *) printf '用法: %s {load|init|up|status|logs|diagnostics|stop}\n' "$0" ;;
esac
