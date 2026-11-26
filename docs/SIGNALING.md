# 信令与会话

> 信令服务器的唯一职责：**让两个端点找到彼此，并在需要时转发密文**。
> 它不参与密钥协商，不解析媒体，不保存画面。

实现：`crates/vr-signal/src/lib.rs`
接口契约：`api/openapi.yaml`

---

## 1. 端口与端点

| 端点 | 协议 | 默认端口 | 用途 |
|---|---|---|---|
| `/v1/*` | HTTP(S) | 34790 | 注册、配对、会话 |
| `/v1/agent/ws` | WebSocket | 34790 | 被控端控制通道（收邀请） |
| `/v1/session/ws` | WebSocket | 34790 | 会话中继（双向密文） |
| UDP reflex | UDP | 34791 | mini-STUN，返回观察到的公网地址 |

生产部署建议：`443` + TLS 终止，HTTP 与 WS 同源。

---

## 2. 数据模型

```
Device
  id            UUID
  info          {device_id, name, platform, version, tags[]}
  token         32 字节随机，bearer，长期
  pairing       (code, created_at)  短时，默认 300 秒
  ws            当前控制通道（可空）
  last_seen

Session
  id            UUID
  device_id     目标设备
  token         32 字节随机，单次会话
  viewer_name   显示用
  state         pending | accepted | active | closed
  created_at
  lan_hints[]   agent 接受时上报的直连地址
  udp_port      可选
  viewer_tx / agent_tx   中继通道
```

存储：v0.1 为进程内 `HashMap`（`RwLock`）。
**重启后设备丢失**，被控端会自动重新注册（见 §6）。
生产多副本需要把 `devices` 换成 PostgreSQL/Redis，`sessions` 保持粘性路由。

---

## 3. REST

所有响应 `application/json`。错误统一：

```json
{"error":{"code":"unauthorized","message":"device token required"}}
```

| 状态码 | code | 场景 |
|---|---|---|
| 400 | `bad_request` | 参数缺失/长度非法 |
| 401 | `unauthorized` | 缺失或错误 token |
| 403 | `forbidden` | 配对码错误或过期 |
| 404 | `not_found` | 设备/会话不存在 |

### 3.1 `POST /v1/devices` — 注册被控端

请求：

```json
{"name":"Studio","platform":"macos-aarch64","version":"0.1.0","tags":["agent"]}
```

响应 `200`：

```json
{"device_id":"0f7c…","token":"9a3e…"}
```

`token` 只在这里返回一次。被控端必须持久化（`identity.json`，权限 0600）。

### 3.2 `GET /v1/devices` — 列设备

需要 `Authorization: Bearer <admin_token 或任一设备 token>`。
未配置 `VELAROUTER_ADMIN_TOKEN` 时开放（仅建议内网）。

响应：`DeviceInfo[]`，**不含 token**。

### 3.3 `POST /v1/devices/{id}/pairing` — 设置配对码

需要设备 token。

```json
{"code":"123456"}
```

响应：`{"ok":true,"ttl_secs":300}`

被控端每 120 秒刷新一次（`PAIRING_REFRESH`），避免码在会话中途过期。

### 3.4 `DELETE /v1/devices/{id}` — 注销

需要该设备 token 或 admin token。用于“远程下线”。

### 3.4.1 `POST /v1/rendezvous` — P2P 会合（主控端）

```json
{"device_code":"851023857"}
```

响应：`{"token":"…","udp_port":34792,"ttl_secs":60}`。
服务器通过被控端控制 WS 推送 `{"t":"rendezvous","token":…}`，双方在 UDP 34792 上交换观察地址。
**密码不参与此请求。**

### 3.5 `POST /v1/sessions` — 创建会话（主控端）

```json
{"device_id":"0f7c…","pairing_code":"123456","viewer_name":"Alice 的 MacBook"}
```

响应 `200`：

```json
{"session_id":"c1a2…","token":"5b7d…","ws_path":"/v1/session/ws?session_id=c1a2…&token=...&role=viewer"}
```

副作用：如果目标设备的控制 WS 在线，会收到：

```json
{"t":"invite","session_id":"c1a2…","viewer":"Alice 的 MacBook"}
```

失败：

- 设备不存在 → 404
- 配对码错误/过期 → 403（不区分“错”与“过期”，避免枚举）

### 3.6 `POST /v1/sessions/{id}/accept` — 接受会话（被控端）

需要设备 token。

```json
{"lan_hints":["ws://192.168.1.10:34789/vr","ws://10.0.0.5:34789/vr"],
 "udp_port":51820}
```

响应：`{"ok":true}`

副作用：若 Viewer 已连上中继，会收到 `{"t":"ready","lan_hints":[...],"udp_port":51820}`。

### 3.7 `GET /v1/sessions/{id}` — 查询状态（主控端）

需要会话 token。

```json
{"session_id":"c1a2…","state":"accepted","viewer":"Alice 的 MacBook",
 "lan_hints":["ws://192.168.1.10:34789/vr"],"udp_port":51820,"age_secs":3}
```

### 3.8 `DELETE /v1/sessions/{id}` — 主动结束

需要会话 token。向两端发送 `Close`，移除会话。

### 3.9 `GET /v1/health`

```json
{"ok":true,"version":1,"uptime_secs":812,"devices":3,"sessions":1}
```

供负载均衡与监控使用。

---

## 4. WebSocket

### 4.1 被控端控制通道 `/v1/agent/ws`

握手请求头：

```
Authorization: Bearer <device_token>
```

服务端 → 客户端：

```json
{"t":"invite","session_id":"c1a2…","viewer":"Alice"}
```

客户端 → 服务端（可选）：

```json
{"t":"ping"}
```

心跳：服务端每 20 秒发 WebSocket Ping；客户端应回 Pong（库自动处理）。
断线后指数退避重连（1s → 30s）。

### 4.2 会话中继 `/v1/session/ws?session_id=…&token=…&role=viewer|agent`

鉴权规则：

| role | token 来源 |
|---|---|
| `viewer` | `POST /v1/sessions` 返回的会话 token |
| `agent` | 该会话所属设备的设备 token |

中继控制消息（文本帧，服务端 → 客户端）：

```json
{"t":"waiting"}                 // 已连接，等待被控端
{"t":"ready","lan_hints":[…],"udp_port":51820}
{"t":"peer_joined"}             // 两端都到位
{"t":"peer_left"}               // 对端断开
```

字节流（二进制帧）：

```
0x01 + JSON    VRP 握手明文
0x02 + bytes   VRP 加密信封
```

服务端原样转发到对端，不解析、不改写、不缓存。
任一端断开，另一端收到 `peer_left` 并在 5 秒内关闭。

---

## 5. 完整时序

### 5.1 跨网段（经中继）

```
Viewer                 Signal                  Agent
  │ POST /v1/sessions     │                      │
  │──────────────────────►│                      │
  │                       │── {"t":"invite"} ───►│
  │ {session_id, token}   │                      │
  │◄──────────────────────│                      │
  │                       │◄── POST .../accept ──│  (lan_hints)
  │ WS /v1/session/ws     │                      │
  │──────────────────────►│◄─ WS /v1/session/ws ─│
  │◄── {"t":"ready"} ─────│──── {"t":"ready"} ──►│
  │                       │                      │
  │ ═══════════ VRP 握手 + 加密会话（中继转发） ═══════════ │
```

### 5.2 同一局域网（不经中继）

```
Viewer                 Signal                  Agent
  │ POST /v1/sessions     │                      │
  │──────────────────────►│── invite ───────────►│
  │◄── session_id ────────│◄─ accept(lan_hints) ─│
  │                       │                      │
  │ 直接连接 ws://192.168.1.10:34789/vr（跳过中继）
  │─────────────────────────────────────────────►│
  │ ═════════════ VRP 握手 + 加密会话 ════════════ │
```

客户端策略：

1. 拿到 `lan_hints` 后，与信令服务器并行地尝试直连；
2. 3 秒内直连握手成功 → 关闭中继连接；
3. 否则保留中继。

这个“并行竞速”避免了“先等直连超时再回落”的 3 秒延迟。

### 5.3 完全离线

不给 agent 配置 `--signal`，直接用 `ws://<ip>:34789/vr` + 配对码。
信令服务器不参与任何环节。

---

## 6. 生命周期与容错

| 事件 | 行为 |
|---|---|
| 信令重启（内存丢失） | Agent 收到 401/404 后重新 `POST /v1/devices`，写回 identity |
| 配对码过期 | Agent 每 120 秒刷新；Viewer 失败后重新读取 |
| 会话 120 秒未被接受 | 清理任务删除 |
| Agent 控制 WS 断开 | 会话仍可通过已建立的中继继续；仅新邀请丢失 |
| Viewer 断开 | Agent 收到 `peer_left`，结束会话任务、停止采集 |
| 中继断开 | 双方各自重连；token 在 TTL 内有效 |
| 服务端 OOM | 单会话中继缓冲上限由 WebSocket 库控制（默认 64 MiB/连接，建议调到 8 MiB） |

---

## 7. 安全边界

- 设备 token 是长期凭据：泄露 = 可冒充被控端。存 0600，支持轮换（删除设备后重注册）。
- 会话 token 是单次凭据：只允许连接对应 `session_id` 的中继。
- 配对码不是凭据，只是密钥派生的一部分；即使服务端被攻破，也无法解密会话。
- 服务端**不能**伪造邀请之外的任何内容：握手有双向证明。
- 中继可以被 DDoS：生产必须限速（每 IP 并发会话数、每秒新建会话数）。

---

## 8. 运维指标建议

| 指标 | 来源 | 告警阈值 |
|---|---|---|
| `devices` | `/v1/health` | 突降 50% |
| `sessions` | `/v1/health` | 持续 > 容量 70% |
| 新建会话失败率 | 访问日志 4xx/5xx | > 5% |
| 中继并发连接 | WS accept 计数 | > 单机上限 80% |
| 中继吞吐 | 网卡 | > 带宽 70% |
| 握手失败率 | Agent 日志 | > 10%（可能被爆破） |

---

## 9. 扩展方向

1. **持久化**：设备表落库，支持多副本；会话用一致性哈希粘性路由。
2. **鉴权升级**：设备 token → mTLS 或 OIDC；管理员按组织隔离设备。
3. **配额**：每设备并发会话数、每会话码率上限。
4. **可观测**：OpenTelemetry trace，把 `session_id` 贯穿两端。
5. **地理路由**：返回最近的中继节点（当前单节点）。
