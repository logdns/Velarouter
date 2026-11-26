# 自建信令服务

**局域网直连不需要部署。** 跨网时可继续用客户端的公共会合服务，或改用自建信令。当前桌面端的自建信令模式用 9 位码建立 QUIC 直连，打洞失败不会自动使用中继。

## 准备

- 一台你管理的公网 Linux 服务器，安装 Docker Engine、Compose v2、curl、openssl。
- 把域名指向服务器；若配置了 AAAA，也要能从公网通过 IPv6 访问。
- 放行 TCP 80/443（Caddy 签发证书与 HTTPS/WSS）和 UDP 34791/34792（地址探测与打洞）。不要把被控端 34789 或信令后台 34790 暴露到公网。

## 部署

在服务器的项目目录执行（替换为自己的域名）：

```sh
bash scripts/deploy-signal.sh signal.example.com
```

脚本启动信令和 Caddy，生成本地管理员令牌，并检查公网 HTTPS。两台电脑在「设置 → 网络 → 自建信令 / 中继服务器」填同一个 HTTPS 地址，再用被控端的 9 位码和密码连接。只使用自己信任的服务：被控端会向它登记配对信息。

需要手动部署时，只构建信令：`cargo build --release --locked -p vr-signal --bin vr-signal`。使用 `VELAROUTER_PUBLIC_URL` 和 `VELAROUTER_ADMIN_TOKEN` 配置服务，端口和反代配置参考 [Compose](../packaging/signal/compose.yaml) 与 [Caddyfile](../packaging/signal/Caddyfile)。

## 维护

```sh
curl -fsS https://signal.example.com/v1/health
(cd packaging/signal && docker compose ps && docker compose logs --tail=80)
(cd packaging/signal && docker compose down) # 停止，保留证书卷
```

升级时用同一域名重跑部署命令。备份 `packaging/signal/.env` 和 Caddy 数据卷，不提交令牌。健康检查失败时查 DNS、80/443 与 Caddy 日志；健康检查通过但无画面时，查两端 UDP 和 [NAT 限制](RENDEZVOUS.md)。服务重启后设备需重新登记，会话需重连。

[信令 API](SIGNALING.md) · [权限与安全边界](SECURITY.md) · [实机验收](clients/LINUX-ACCEPTANCE.md)
