# 内网直连、NAT 穿透与中继

> “能否直连”决定体验，“直连失败能否自动回落”决定可用性。
> 本文描述 Velarouter 的连接决策、发现协议、打洞原语与排障方法。

实现：`crates/vr-net/src/lib.rs`、`crates/vr-session/src/lib.rs`、`crates/vr-agent/src/server.rs`

---

## 1. 连接优先级

```
1. 用户手工指定的 URL（最高优先，便于排障）
2. 局域网直连（lan_hints + 广播发现）
3. UDP 打洞（双方公网地址可达时）
4. 信令中继（永远可用）
```

客户端的标准流程：

```
获取设备信息
   │
   ├─ 有 lan_hints? ──► 并行尝试所有 hint（300ms 超时）
   │                        成功 → 用直连
   │
   ├─ 有公网映射?   ──► 同时向对端公网地址发 UDP probe
   │                        收到 ack → 用 UDP 直连
   │
   └─ 以上都失败    ──► 连接中继，等待 ready
```

**直连与中继并行发起**，谁先握手成功用谁，另一个立即关闭。
这比“先等 3 秒再回落”少 3 秒首帧延迟。

---

## 2. 局域网发现

### 2.1 广播探测

UDP 广播到 `255.255.255.255:34789`：

```
请求： "VRDISC1"                                  (7 bytes)
响应： "VRDISC1R" + UTF-8 JSON(DeviceInfo)         (7 + N bytes)
```

`DeviceInfo`：

```json
{"device_id":"0f7c…","name":"Studio","platform":"macos-aarch64",
 "version":"0.1.0","tags":["agent"]}
```

发送方连发 3 次（间隔 60ms），收集 3 秒内的所有响应，按 `device_id` 去重。

实现细节：

- socket 打开 `SO_BROADCAST`；
- 不绑定固定端口，用临时端口发送；
- 响应里**不含配对码、不含 token**，只用于 UI 列表。

### 2.2 mDNS（未实现，预留）

跨 VLAN 且禁止广播的企业网，可加 `_velarouter._tcp.local` 的 mDNS 广告。
接口已留：`vr_net::Discovery` 可替换实现，不影响上层。

### 2.3 LAN hints

被控端接受会话时上报：

```json
["ws://192.168.1.10:34789/vr", "tcp://192.168.1.10:34789",
 "ws://10.0.0.5:34789/vr",     "tcp://10.0.0.5:34789"]
```

地址由 `if-addrs` 枚举，过滤 loopback 与 IPv6 link-local，按“私有地址优先”排序，最多 8 条。

---

## 3. mini-STUN

公网地址发现不依赖外部 STUN 服务，信令服务器自带一个 12 行的反射器。

请求：

```
"VRREF1"                                           (6 bytes)
```

响应：

```
"VRREF1R" + ip_len:u8 + ip_ascii + port:u16(BE)
```

示例（`203.0.113.7:45678`）：

```
56 52 52 45 46 31 52  0D  32 30 33 2E 30 2E 31 31 33 2E 37  B2 6E
└──── "VRREF1R" ────┘ ip_len  └────── ip ───────────────┘  port
```

为什么自己实现：

- 对外部 STUN 的依赖会引入第三方可用性问题；
- 只有“观察到的地址”这一个需求，不需要 STUN 的完整属性集；
- 被控端也可以充当反射器（离线环境）。

---

## 4. UDP 打洞

两端同时向对方的公网地址发探测包：

```
probe:  "VRPUNCH"
ack:    "VRPUNCHK"
```

流程（`vr_net::punch` + `vr_net::answer_punch`）：

```
A                         NAT-A                    NAT-B                         B
│  bind 0.0.0.0:0           │                        │                          │
│  connect B_pub            │                        │                          │
│──── VRPUNCH ─────────────►│──── 创建映射 ──────────►│──── VRPUNCH ────────────►│
│                           │                        │                          │
│◄──────────────────────────│◄──── 放行（有映射） ────│◄──── VRPUNCHK ───────────│
│  收到 ack → 打洞成功        │                        │                          │
```

参数：

| 参数 | 建议值 | 说明 |
|---|---|---|
| 探测间隔 | 120 ms | 太快会触发 NAT 限速 |
| 总超时 | 3 s | 超过即回落中继 |
| 载荷 | 7 / 8 字节 | 越小越不容易被分片 |

适用性：

| NAT 类型 | 结果 |
|---|---|
| 全锥 / 受限锥 | ✅ 通常成功 |
| 端口受限锥 | ✅ 同时发包时成功 |
| 对称 NAT | ❌ 端口不可预测，回落中继 |
| CGNAT | ⚠️ 取决于运营商，多数需要中继 |
| 企业防火墙（只出 443） | ❌ 回落 WSS 中继 |

打洞失败不是错误，是常态；**中继必须永远可用**。

---

## 5. 中继

中继是一个 WebSocket 字节管道：

- 两端用各自的 token 连接 `/v1/session/ws`；
- 服务端按 `session_id` 把二进制帧原样复制到对端；
- 文本帧只用于中继自身控制；
- 不解析、不缓存、不落盘。

带宽成本：

| 场景 | 1600×900 q60 | 1080p q70 |
|---|---|---|
| 静态办公 | ~0（跳过不变帧） | ~0 |
| 文字/网页 | 1–3 Mbps | 3–6 Mbps |
| 视频/游戏 | 8–15 Mbps | 15–30 Mbps |

中继节点建议：

- 每会话 ≤ 20 Mbps，单机 200 会话需要 ≥ 5 Gbps 网卡；
- 开启 `TCP_NODELAY`，关闭 Nagle；
- 反向代理（Nginx/Caddy）需 `proxy_read_timeout` ≥ 1h，并允许 WebSocket upgrade。

---

## 6. 端口与防火墙

| 方向 | 端口 | 协议 | 必须? |
|---|---|---|---|
| 被控端入站 | 34789 | TCP (WS) | 仅局域网直连需要 |
| 被控端入站 | 34789 | UDP (发现) | 可选 |
| 被控端出站 | 443 | TCP (WSS 中继) | 公网模式必须 |
| 信令入站 | 443/34790 | TCP (HTTPS/WS) | 必须 |
| 信令入站 | 34791 | UDP (反射) | 打洞时需要 |

**最保守的企业部署**：只开放信令 443，被控端主动外连，客户端也主动外连，
全部走中继——没有入站端口，也不需要 UDP。

---

## 7. 排障

### 7.1 先确认是不是局域网

```bash
velarouter discover --seconds 3
```

能看到设备 → 广播可用；看不到 → 广播被拦或跨 VLAN，用 `--url` 直连。

### 7.2 直连测试

```bash
velarouter probe --url ws://192.168.1.10:34789/vr --code 123456 --frames 3
```

输出 `handshake ok` 即协议可达；没有输出说明 TCP 不通或被防火墙拦截。

### 7.3 端口检查

```bash
nc -vz 192.168.1.10 34789        # macOS/Linux
Test-NetConnection 192.168.1.10 -Port 34789   # Windows
```

### 7.4 反射服务检查

```bash
# 服务器上
velarouter signal --bind 0.0.0.0:34790 --reflex-port 34791

# 客户端
python3 - <<'PY'
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(2)
s.sendto(b"VRREF1", ("signal.example.com", 34791))
data, _ = s.recvfrom(128)
assert data.startswith(b"VRREF1R"), data
n = data[6]
print(data[7:7+n].decode(), int.from_bytes(data[7+n:9+n], "big"))
PY
```

### 7.5 常见现象

| 现象 | 可能原因 | 处理 |
|---|---|---|
| 局域网能看到设备但连不上 | 被控端监听在 127.0.0.1 | `--host 0.0.0.0` |
| 握手很慢 | DNS 或代理拦截 WS | 直接用 IP + `ws://` |
| 连上几秒后断开 | 中间设备回收空闲连接 | 客户端 30 秒 ping 一次 |
| 画面全黑 | 采集权限缺失 | 看 `warning{code:"capture"}` |
| 画面糊 | 带宽不足 + 质量高 | 降 `max_width` / `quality` |
| 输入无反应 | 注入权限缺失 | 看 `warning{code:"input"}`；macOS 辅助功能 |
| 中继 1006 | 反代超时 | 调大 `proxy_read_timeout` |

---

## 8. 延迟预算

局域网 1 Gbps，1600×900 q60：

| 阶段 | 耗时 |
|---|---|
| 采集（xcap） | 5–15 ms |
| JPEG 编码 | 8–20 ms |
| AES-GCM 加密 | < 1 ms |
| 传输 | < 2 ms |
| 解密 + 解码 | 3–8 ms |
| 渲染 | < 2 ms |
| **合计** | **20–50 ms** |

公网经中继增加 2×RTT；跨洲 RTT 200 ms 时，端到端约 450 ms——这就是为什么
公网场景必须降低分辨率与帧率，并在 roadmap 中引入 UDP 媒体。

---

## 9. 未来工作

1. **UDP 媒体通道**：`FragmentHeader` 与 `Reassembler` 已就绪，缺发送调度、FEC、带宽估计。
2. **ICE-lite 候选**：把 `lan_hints` 升级为正式候选（host/srflx/relay），支持增量更新。
3. **多中继选路**：信令返回最近节点列表，客户端测 RTT 选优。
4. **QUIC 通道**：把 `MessageChannel` 换成 QUIC stream，天然多路复用 + 0-RTT。
5. **mDNS**：跨 VLAN 企业网。
