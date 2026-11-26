#!/usr/bin/env bash
# Run on the intended public server, from an audited repository checkout.
set -euo pipefail
cd "$(dirname "$0")/../packaging/signal"
export LC_ALL=C
domain=${1:-}
if [[ $# != 1 || ${#domain} -gt 253 || ! $domain =~ ^([A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+[A-Za-z]{2,63}$ ]]; then
    echo '用法: bash scripts/deploy-signal.sh signal.example.com（域名，不含协议或端口）' >&2
    exit 2
fi
for tool in docker openssl curl; do command -v "$tool" >/dev/null || { echo "缺少 $tool" >&2; exit 1; }; done
docker compose version >/dev/null
docker info >/dev/null
umask 077
if [[ ! -e .env ]]; then
    tmp=$(mktemp ./.env.XXXXXX)
    trap 'rm -f "$tmp"' EXIT
    token=$(openssl rand -hex 32)
    printf 'SIGNAL_DOMAIN=%s\nVELAROUTER_ADMIN_TOKEN=%s\n' "$domain" "$token" > "$tmp"
    ln "$tmp" .env  # atomic; never overwrite an existing admin token
fi
if ! grep -Fxq "SIGNAL_DOMAIN=$domain" .env; then
    echo '域名与已部署配置不同；如需迁移请先检查 packaging/signal/.env' >&2; exit 1
fi
unset SIGNAL_DOMAIN VELAROUTER_ADMIN_TOKEN
docker compose --env-file .env up -d --build
url="https://$domain"
if ! curl -fsS --retry 15 --retry-delay 3 --retry-all-errors --max-time 5 "$url/v1/health"; then
    echo 'HTTPS 健康检查失败。检查 DNS、TCP 80/443、防火墙及 docker compose logs。' >&2; exit 1
fi
printf '\n两端客户端 → 设置 → 网络 → 自建信令 / 中继：%s\n' "$url"
