# 互联网直连（P2P，无中继）

> 桌面客户端默认走的是**公共会合 + 打洞**这条路径，配置与失败场景见
> [会合与直连](RENDEZVOUS.md)。本文记录协议层细节与自建信令服务器版本。

> 目标：安装即用，客户端随机生成 **联机编码** 与 **联机密码**；
> 主控端只输入这两个值，双方通过 **UDP 打洞 + QUIC** 直连。
> **没有中继兜底**：打不通就明确报错，而不是悄悄降级。

参考：UUYC 官网的“ID 与验证码快速连接”“端口映射”，以及 TeamViewer 式的免配置模型。

---

## 1. 体验

被控端启动后自动注册，打印：

```
── 互联网直连（无中继 / P2P）────────────────────────────
联机编码 / Connection ID : 851023857
联机密码 / Password      : 445566
连接方式                  : TCP/UDP 打洞 + QUIC（失败即报错）
─────────────────────────────────────────────────────────
```

主控端：

```bash
velarouter connect \
  --signal https://signal.example.com \
  --code 851023857 \
  --password 445566 \
  --save screenshot.jpg
```

输出：

```
rendezvous token acquired; punching UDP to 203.0.113.5:34792
punched: local port 55699 → peer 198.51.100.7:57000 (no relay)
P2P session established: device=Some("Studio")
first frame after 181 ms: 1920x1080 106404 bytes
received 3 frames, 1920x1080 in 445 ms (P2P/QUIC, no relay)
```

---

## 2. 编码与密码从哪来

| 值 | 生成方 | 生命周期 | 说明 |
|---|---|---|---|
| 联机编码 | 信令服务器 | 与设备注册绑定 | 9 位数字，全局唯一；用于找到设备 |
| 联机密码 | 被控端 | 每次启动随机（可用 `--code` 固定） | 6 位数字；**不经过信令服务器** |

- 编码不是秘密（相当于 TeamViewer ID）；密码才是。
- 密码不落服务器、不进日志、不进 URL。
- 公网场景建议用 `--code` 指定 8 位以上随机串，或定期重启轮换。
- 连续失败 5 次后，被控端应退避（当前实现失败即断开，建议加指数退避）。

---

## 3. 协议流程

```
Agent                                  Signal (HTTP + UDP)                    Viewer
  │  POST /v1/devices                      │                                    │
  │ ──────────────────────────────────────►│  device_id + token + device_code   │
  │                                        │                                    │
  │  WS /v1/agent/ws (token)               │                                    │
  │ ◄──────────────────────────────────────│                                    │
  │                                        │◄── POST /v1/rendezvous {code} ─────│
  │ ◄── WS {"t":"rendezvous","token"} ─────│──── {token, udp_port} ────────────►│
  │                                        │                                    │
  │  UDP: VRRG1 A <token> ────────────────►│◄── VRRG1 V <token> ────────────────│
  │ ◄── VRRG1P <viewer_ip:port> ───────────│──── VRRG1P <agent_ip:port> ───────►│
  │                                        │                                    │
  │  VRPUNCH ⇄ VRPUNCHK（同一个 UDP socket）│                                    │
  │                                        │                                    │
  │  QUIC 握手（同一 socket，agent 为服务端）│                                    │
  │ ◄══════════ VRP 握手（密码）═══════════════════════════════════════════════►│
  │ ◄══════════ 加密会话（单条双向 QUIC stream）═══════════════════════════════►│
```

关键点：

1. **信令服务器只做介绍人**：返回双方观察到的公网 UDP 地址，不接触密码。
2. **打洞与 QUIC 共用同一个 socket**：NAT 映射不变，QUIC 直接复用已打通的五元组。
3. **agent 是 QUIC 服务端**：自签证书，每个进程生成；客户端跳过证书校验（见 §5）。
4. **身份由 VRP 握手证明**：密码进 HKDF salt，agent/viewer 双向 HMAC proof。
5. **一条双向 QUIC stream 承载整个 VRP 会话**：`len u32 | kind u8 | payload`。

---

## 4. 为什么用 QUIC 而不是裸 UDP

| 需求 | 裸 UDP | QUIC |
|---|---|---|
| 可靠有序的控制消息 | 自己写重传/排序 | 内置 |
| 拥塞控制 | 自己写 BBR/Cubic | 内置 |
| 加密 | 已有 AES-GCM | TLS1.3 + 我们的 AES-GCM（双层） |
| 连接迁移 | 无 | 支持（换网不断） |
| NAT 保活 | 自己写 | keep-alive 配置 |
| 代码量 | ~600 行且难测 | ~250 行封装 |

当前媒体也走同一条 stream（可靠有序）。下一阶段用 QUIC datagram 承载 P 帧，
关键帧仍走 stream，即可获得“可用即低延迟”的分级传输，详见 `OPEN-SOURCE.md` §1。

---

## 5. 安全说明（重要）

- QUIC 的证书是**临时自签**的，客户端**跳过校验**。
  这不是漏洞：证书不承担身份，身份由 VRP 的密码握手证明。
  一个中间人即便终止 QUIC，也无法伪造 `HMAC(key, "agent|session")`，因为它不知道密码。
- 密码是 6 位数字（约 20 bit）。公网部署建议使用 `--code` 指定长密码；
  并确保被控端对失败尝试做退避。
- 信令服务器可以看到“谁在什么时候尝试连接谁”，但看不到密码、画面、输入与文件。
- 打洞失败时**不会**回落到服务器：这是刻意的产品选择，避免用户误以为“直连”却在绕路。

---

## 6. NAT / 防火墙矩阵

| 网络 | UDP 打洞 | 结果 |
|---|---|---|
| 同一局域网 | 不需要 | 优先用 `ws://ip:34789/vr` 直连 |
| 全锥 / 受限锥 NAT | ✅ | 通常成功 |
| 端口受限锥 NAT | ✅ | 同时发包时成功 |
| 对称 NAT | ❌ | 失败并报错；需要端口转发或换网络 |
| CGNAT | ⚠️ | 多数失败 |
| 只允许 TCP 443 的企业网 | ❌ | 失败；建议使用带中继的部署（本项目不默认启用） |

需要的端口：

| 方向 | 端口 | 协议 | 用途 |
|---|---|---|---|
| 出站 | 34792 | UDP | 会合（观察公网地址） |
| 双向 | 临时高位端口 | UDP | 打洞 + QUIC |
| 出站 | 443/34790 | TCP | 注册、会话撮合 |
| 入站（可选） | 34789 | TCP | 局域网直连 |

---

## 7. 与中继模式的关系

| 模式 | 何时用 | 代码 |
|---|---|---|
| 局域网直连 | 同网段 | `ws://ip:34789/vr` |
| **P2P（本文）** | 公网、NAT 可打洞 | `velarouter connect` |
| 中继 | 对称 NAT / 企业防火墙 | 信令 `/v1/session/ws`（**默认不自动回落**） |

产品策略：默认 P2P，失败明确提示；需要中继的部署显式开启（`--relay` 或服务端策略）。

---

## 8. 代码位置

| 模块 | 文件 |
|---|---|
| 会合协议 + 打洞 + QUIC 封装 | `crates/vr-p2p/src/lib.rs` |
| 信令：设备编码 / 会合 HTTP / UDP 服务 | `crates/vr-signal/src/lib.rs` |
| 被控端：WS 收到 rendezvous → 接受 | `crates/vr-agent/src/signal.rs` |
| 主控端：`connect` 命令 | `crates/velarouter/src/main.rs` |
| 密码 / 编码展示 | `crates/vr-agent/src/signal.rs` |

---

## 9. 已知限制与下一步

1. 媒体走可靠 stream，弱网下会被队头阻塞 → 改用 QUIC datagram + 关键帧 stream。
2. 对称 NAT 无法直连，且没有中继兜底 → 文档已明确；需要时显式启用中继。
3. 6 位密码需要退避与锁定策略 → 建议在 agent 增加失败计数。
4. 证书跳过校验依赖 VRP 密码强度 → 公网建议长密码。
5. 尚未做连接迁移测试（Wi-Fi ↔ 5G）→ QUIC 原生支持，补测试即可。

---

## 10. 实测记录

同机（回环）验证：

```
punched: local port 55699 → peer 127.0.0.1:57000 (no relay)
P2P session established: device=Some("ideadevdeMac-Studio.local")
first frame after 181.9 ms: 1920x1080 106404 bytes
received 3 frames, 1920x1080 in 445.0 ms (P2P/QUIC, no relay)
```

跨真实 NAT 需要在两台不同网络的机器上验证；代码路径与端口已就绪。
