# 架构

> 本文描述 Velarouter 的**目标架构**，并解释每个决定背后的取舍。
> 本文描述 Velarouter 的模块分工与设计。当前安装和使用以[桌面指南](clients/DESKTOP-APP.md)为准。

---

## 1. 目标与非目标

### 目标

1. **局域网优先**：同一网段内不依赖任何服务器，延迟由交换机决定。
2. **端到端加密**：中继、信令、日志都看不到画面、输入、剪贴板。
3. **协议可被第三方实现**：消息是 JSON，媒体是固定头 + JPEG；任何语言 200 行内能跑通握手。
4. **平台能力可插拔**：采集、编码、注入是三个 trait，换平台不改核心。
5. **可运维**：一个二进制、一个配置文件、结构化日志、健康检查。
6. **可私有化**：没有账号体系、没有云依赖、没有遥测。

### 非目标（当前阶段）

- 不做完整 WebRTC（不实现 SRTP/拥塞控制/Simulcast）。
- 不做游戏级虚拟按键与宏（属于上层输入编排）。
- 不做云电脑编排、会员计费、活动运营。
- 不做文件传输与终端（协议预留 `Control` 扩展位）。

这些不是“永远不做”，而是“不在 v0.1 阻塞核心路径”。

---

## 2. 分层

```
┌──────────────────────────────────────────────────────────────────┐
│ 角色层                                                            │
│  ┌────────────┐   ┌──────────────┐   ┌────────────────────────┐  │
│  │ vr-signal  │   │  vr-agent    │   │ velarouter (client/CLI)│  │
│  │ 注册/配对   │   │ 采集/编码/注入│   │ 握手/解码/输入          │  │
│  └─────┬──────┘   └──────┬───────┘   └───────────┬────────────┘  │
├────────┼─────────────────┼───────────────────────┼───────────────┤
│ 会话层 │                 │                       │               │
│        └──────────┬──────┴───────────┬───────────┘               │
│              ┌────▼─────┐      ┌──────▼──────┐                   │
│              │vr-session│◄────►│  vr-crypto  │                   │
│              │握手/封装  │      │ECDH/HKDF/GCM│                   │
│              └────┬─────┘      └─────────────┘                   │
├───────────────────┼──────────────────────────────────────────────┤
│ 传输层             │                                              │
│      ┌────────────▼────────────┐      ┌─────────────────────┐    │
│      │     vr-transport        │◄────►│       vr-net        │    │
│      │ 分帧/分片/双工通道       │      │ 发现/反射/打洞       │    │
│      └────────────┬────────────┘      └─────────────────────┘    │
├───────────────────┼──────────────────────────────────────────────┤
│ 协议层             │                                              │
│              ┌────▼─────┐                                        │
│              │ vr-proto │  Control 枚举 + VideoHeader            │
│              └──────────┘                                        │
└──────────────────────────────────────────────────────────────────┘
```

依赖方向严格单向：`proto ← transport ← session ← roles`，`crypto` 只依赖 `proto` 的常量。
没有任何角色层反过来污染协议层的情况。

---

## 3. 三个部署形态

### 3.1 纯局域网（默认）

```
┌──────────┐        ws://192.168.1.10:34789/vr        ┌──────────┐
│ Viewer   │ ───────────────────────────────────────► │  Agent   │
│ (浏览器)  │ ◄─────────────────────────────────────── │ (被控端)  │
└──────────┘        加密 JSON + 加密 JPEG 帧           └──────────┘
     ▲                                                      ▲
     │  用户从被控端屏幕读取 6 位配对码                        │
     └──────────────────────────────────────────────────────┘
```

- 没有 DNS、没有账号、没有外网；
- 配对码只通过“人眼”传输；
- WebSocket 在局域网内一次握手，之后全是二进制密文。

### 3.2 经信令服务器（跨网段）

```
                   ┌──────────────────┐
                   │   vr-signal      │
                   │ 设备表/配对/中继   │
                   └───┬──────────┬───┘
        POST /v1/sessions│          │WS /v1/agent/ws
                   ┌────▼───┐  ┌───▼────┐
                   │ Viewer │  │ Agent  │
                   └────┬───┘  └───┬────┘
                        │ 直连成功? │
             ┌──────────┴──────────┴──────────┐
             │ 是: 局域网/打洞直连              │ 否: WS 中继（密文）
             └────────────────────────────────┘
```

信令服务器的职责被刻意压到最小：**它只负责让两个端点找到彼此**。
媒体永远不经过业务逻辑，中继只是字节转发。

### 3.3 无信令 + 反向代理（企业）

信令服务器放在内网，公网只暴露 `wss://`；被控端主动外连，不需要开放入站端口。
这是唯一推荐给企业的形态。

---

## 4. Crate 职责

| Crate | 行数量级 | 职责 | 不做什么 |
|---|---|---|---|
| `vr-proto` | ~700 | 消息定义、帧头、坐标归一化 | 不做 IO、不做加密 |
| `vr-crypto` | ~350 | P-256 ECDH、HKDF、AES-GCM、证明 | 不做传输、不做存储 |
| `vr-transport` | ~500 | 长度前缀、UDP 分片/重组、双工 trait | 不知道业务消息 |
| `vr-net` | ~450 | 地址枚举、发现、mini-STUN、打洞 | 不持有会话 |
| `vr-session` | ~900 | 握手状态机、信封、心跳、可拆分收发 | 不做采集/渲染 |
| `vr-signal` | ~800 | 设备表、配对、会话、中继 | 不看媒体内容 |
| `vr-agent` | ~1100 | 采集、编码、注入、剪贴板、LAN 服务 | 不做 UI、不做账号 |
| `velarouter` | ~500 | CLI、probe、doctor | 不含业务逻辑 |

---

## 5. 一次会话的完整时序

```
Viewer                                      Agent
  │                                            │
  │  用户输入配对码 code，生成 session_id        │
  │                                            │
  │ ── Hello{version,role,session_id} ───────► │  校验版本/角色
  │ ── AuthInit{viewer_pub, nonce} ──────────► │
  │                                            │  shared = ECDH(agent_priv, viewer_pub)
  │                                            │  key    = HKDF(shared, sid|code)
  │ ◄──────── AuthFinish{agent_pub, proof} ─── │  proof = HMAC(key,"agent|sid")
  │  shared = ECDH(viewer_priv, agent_pub)     │
  │  key    = HKDF(shared, sid|code)           │
  │  校验 proof                                │
  │ ── AuthProof{HMAC(key,"viewer|sid")} ────► │  校验 proof
  │ ◄──────────────── AuthOk ───────────────── │
  │                                            │
  │ ═════════ 从此开始，双向 AES-256-GCM ═══════ │
  │                                            │
  │ ── Caps{codecs, limits} ─────────────────► │
  │ ◄──────────────── Caps{codecs, device} ─── │
  │ ◄──────────────── DisplayList ──────────── │
  │ ── VideoConfig{jpeg,q60,30fps} ──────────► │
  │                                            │  启动采集线程
  │ ◄──────────── Media frame (JPEG, 24B 头) ── │  变化才发
  │ ── Input{mouse_move/key/wheel} ──────────► │  enigo 注入
  │ ── Clipboard{viewer_to_agent} ───────────► │  arboard 写入
  │ ◄──────────────── Stats{fps,kbps,rtt} ──── │  每秒
  │ ◄──────────────── Ping / Pong ───────────── │  双向
  │ ── Bye / Close ──────────────────────────► │
```

握手阶段只有 `Hello` / `Auth*` 是明文；`Caps` 已经是密文。
这样设计的原因：密钥派生必须发生在任何能力信息交换之前，避免“先泄露能力再认证”。

---

## 6. 线程与并发模型

| 工作 | 执行体 | 原因 |
|---|---|---|
| 屏采 + JPEG 编码 | 每个会话一个 `std::thread` | 编码是 CPU 密集且阻塞的，不能占用 tokio worker |
| 帧投递 | `tokio::mpsc::channel(2)` + `try_send` | 最新帧优先，绝不排队陈旧画面 |
| 会话收发 | 拆成 `SessionSender` / `SessionReceiver` 两个任务 | 收视频与发输入必须并行 |
| 输入注入 | 会话任务内同步调用 | enigo 调用是微秒级，且需要保持顺序 |
| 剪贴板 | `spawn_blocking` | 系统剪贴板 API 可能阻塞 |
| 信令 | 每设备一个控制 WS + 每会话一个中继 WS | 控制面与数据面隔离 |

**关键点**：`Session` 的 `split()` 不是可选优化，而是并发正确性的前提。
一个 `&mut Session` 无法同时 `select!` 接收视频和发送输入——这是许多自制远程桌面在 30fps 时开始卡顿的根因。

---

## 7. 为什么不用 WebRTC

UUYC 用 WebRTC 是合理的：它要跨公网、要抗丢包、要做语音、要兼容海量网络。
Velarouter v0.1 的优先目标是 **可理解、可私有化、可被第三方实现**。

| 维度 | WebRTC | VRP/1 |
|---|---|---|
| 代码量 | 数百万行 C++ | ~4000 行 Rust |
| 依赖 | 原生库、平台适配、证书 | tokio + 标准库 |
| 跨语言实现 | 需要完整栈 | JSON + 固定头 |
| 抗丢包 | FEC/NACK/拥塞控制 | v0.1 无（TCP 可靠传输） |
| 延迟 | 更好（UDP + jitter buffer） | 局域网够用，公网靠中继 |
| 可审计 | 极难 | 全量 |

演进路径写在 [`ROADMAP.md`](ROADMAP.md)：v0.2 增加 UDP 媒体 + 前向纠错，v0.3 接入硬件编码，
但**不引入 WebRTC 的复杂度**，而是按需实现 NACK/FEC/带宽估计的最小集合。

---

## 8. 关键设计决定

### 8.1 控制用 JSON，媒体用二进制

- JSON 让协议可读、可调试、可被脚本生成；输入事件 60Hz 也不过 ~200 字节/事件。
- 媒体帧绝不 JSON/base64：24 字节头 + 原始 JPEG，零拷贝友好。
- 两者共用一个加密计数器，避免双通道乱序。

### 8.2 配对码进入 HKDF salt

```
key = HKDF-SHA256(
  ikm  = ECDH(priv, peer_pub),
  salt = session_id || "|" || pairing_code,
  info = "velarouter/vrp1/session-key"
)
```

- 被动监听者拿不到 key；
- 主动 MITM 不知道配对码就无法构造 `proof`；
- 配对码只有 6 位，所以限制尝试次数（每次握手失败即断开），并对公网场景建议固定长密钥。

### 8.3 双向证明

Agent 先证明自己（`AuthFinish`），Viewer 再证明（`AuthProof`）。
顺序很重要：如果 Viewer 先证明，恶意服务器可以收集 proof 做离线爆破。
当前顺序下，攻击者必须先有一个合法的 agent 私钥才能进入第二步。

### 8.4 计数器隐式、不允许跳号

每条消息的 nonce = `direction(1) || 0,0,0 || counter(8)`。
接收方要求 `counter == expected`，否则直接断开。

- 在有序通道（WS/TCP）上，跳号意味着丢帧或注入；
- 重放会被拒绝（测试覆盖）；
- 代价是：UDP 媒体需要独立 nonce 方案（见 `vr-transport::FragmentHeader.nonce`）。

### 8.5 能力协商而非能力下发

```
Viewer Caps ──► Agent 选择 codec = first(viewer.codecs ∩ agent.codecs)
Agent  Caps ──► Viewer 决定是否启用剪贴板/多屏
```

不依赖服务端下发 JSON 文件，离线也能正确降级。

### 8.6 中继不解析

中继只识别两种帧：文本（中继控制）和二进制（带 1 字节前缀的 VRP 帧）。
它不解析 protobuf/JSON，不知道哪一帧是视频。

前缀约定：

```
0x01 + UTF-8 JSON   → VRP 明文握手
0x02 + bytes        → VRP 密文帧
```

客户端在把 VRP 消息交给中继前自己加前缀，中继原样转发。

---

## 9. 数据流细节

### 9.1 采集

```
xcap::Monitor::capture_image()  →  RgbaImage (2560×1440)
        │
        ├─ 16×16 灰度感知哈希（4bit/像素）→ 与上一帧比较
        │     相同 → 直接返回 None（不编码、不发送）
        │
        ├─ 等比缩放到 max_width × max_height（不放大）
        ├─ RGBA → RGB
        └─ JPEG 编码（quality 20..95）
```

静态桌面几乎零带宽；鼠标移动、光标闪烁会触发重发。
这是“感知哈希”而不是逐像素比较：成本是 256 次采样，收益是整帧编码的节省。

### 9.2 输入

```
浏览器 mousemove
  → 归一化到 0..65535（与分辨率解耦）
  → InputEvent::MouseMove
  → AES-GCM
  → Agent: display 矩形查找 → 反归一化 → enigo.move_mouse(绝对坐标)
```

归一化的好处：主控端不需要知道被控端分辨率，分辨率变化时无需重新协商坐标系。

### 9.3 剪贴板

- 双向，但每条消息带 `direction`，接收方只接受与自己相反的方向，天然防回环；
- Agent 每秒轮询一次系统剪贴板（macOS 无变更通知），与上次发送内容比较；
- 大文本（>1 MiB）会被 `MAX_CONTROL_BYTES` 拒绝——这是刻意的，剪贴板不是文件传输通道。

---

## 10. 错误处理与降级

| 场景 | 行为 |
|---|---|
| 采集权限缺失 | 会话建立但发 `Warning{code:"capture"}`，画面黑屏，UI 提示授权 |
| 注入权限缺失 | 发 `Warning{code:"input"}`，退化为只读会话 |
| 编码器不支持 | 回落到 JPEG 并发 `Warning{code:"codec"}` |
| 中继断开 | 客户端指数退避重连；会话 token 在 TTL 内可复用 |
| 配对码错误 | 双方都在 proof 阶段拒绝，连接立即关闭，不泄露“错在哪一步” |
| 版本不匹配 | `AuthFail{reason:"unsupported version N"}` |
| 会话 ID 不匹配（显式指定时） | `AuthFail`，防止 token 串会话 |
| 帧计数器跳号 | 解密直接失败 → 关闭连接（不尝试恢复，避免 nonce 重用） |

---

## 11. 扩展点

### 11.1 换编码器

实现 `vr_agent::capture::FrameSource`：

```rust
pub trait FrameSource: Send {
    fn displays(&self) -> Vec<DisplayInfo>;
    fn next_frame(&mut self) -> Result<Option<EncodedFrame>>;
    fn select_display(&mut self, id: u32);
    fn set_quality(&mut self, quality: u8);
    fn set_limits(&mut self, max_width: u16, max_height: u16, fps: u8);
}
```

H.264 实现只需把 `VideoHeader.codec` 设为 `H264`，并在 `Capabilities.codecs` 中声明。
协议、加密、传输都不需要改。

### 11.2 换输入后端

实现 `vr_agent::input::Injector`，移动端用无障碍/私有 API，桌面用 enigo。

### 11.3 新消息类型

在 `vr_proto::Control` 加一个 variant，`#[serde(tag="t")]` 自动生成 JSON。
旧客户端遇到未知 `t` 应忽略并记录，而不是断开——这是前向兼容的约定。

### 11.4 新传输

实现 `MessageChannel`（4 个方法 + `split`），即可复用整个会话层。
QUIC、串口、蓝牙 RFCOMM 都只是另一个 `MessageChannel`。

---

## 12. 与 UUYC 的结构对照

| 关注点 | UUYC | Velarouter |
|---|---|---|
| 媒体栈 | libwebrtc（C++，静态链接） | 自研轻量栈（Rust） |
| 协议 | Wire protobuf，170 个类，无文档 | JSON + 24B 二进制头，有规范 |
| 房间 | 业务服务分配，字段含商业化 | 信令服务器撮合，字段最小 |
| 中继 | TURN | 密文 WS 管道 |
| 平台能力 | 下发 JSON 能力表 | 运行时 `Capabilities` 协商 |
| 采集 | 平台原生 + 自研 hook | `xcap` + trait |
| 输入 | 原生 + 无障碍 + MuMu JNI | `enigo` + trait |
| 进程模型 | 6 进程（Win）/ 4 可执行（macOS） | 1 进程（可按需拆分） |
| 可观测 | Sentry + 自研上报 | tracing + `/healthz` + `Stats` 消息 |

---

## 13. 性能基线

| 指标 | 实测（macOS / arm64 / release） |
|---|---|
| 首次出帧 | 177 ms（1600×900） |
| 帧大小 | ~117 KB @ q60 1600×900 |
| 静态画面带宽 | 0（感知哈希跳过） |
| 握手耗时 | < 5 ms（本机） |
| RTT（本机） | 1–5 ms |
| 内存（空载） | ~30 MB |
| 二进制大小 | 9.0 MB（含 reqwest/rustls/xcap/enigo） |

调优建议：

- 公网中继把 `max_width` 降到 1280，`quality` 55–65；
- 静态办公场景 `fps` 设为 15 足够，交互场景 30；
- 游戏/视频场景需要硬件编码，见 [`clients/`](clients/)。

---

## 14. 刻意留下的空白

这些空白是“还没做”，不是“设计没想到”：

1. **UDP 媒体通道**：`vr-transport` 已有分片/重组与独立 nonce，缺的是发送端调度与拥塞控制。
2. **音频**：协议有 `Capabilities.audio`，缺采集与 Opus 编码。
3. ~~**文件传输**~~：已实现（`kind=3` 文件块、断点续传、SHA-256、路径沙箱），见 `PROTOCOL.md` §5.3/§6.9。
4. **多屏同时推流**：列表与切换已实现；并发多路需要按显示器拆分编码器。
5. **终端**：设计见 `OPEN-SOURCE.md` §5（PTY 原始字节流 + 会话保活）。
6. **虚拟按键**：纯客户端功能，不涉及协议。

逐项的选型、代码骨架与验收标准见 [`OPEN-SOURCE.md`](OPEN-SOURCE.md)。

---

## 11. 会合层（`vr-meet`）

2024 版之后新增的一层，位于 `vr-net` 与 `vr-p2p` 之间：

| 模块 | 职责 | 关键决定 |
|---|---|---|
| `vr-meet::broker` | 极简 MQTT 3.1.1 over TLS WebSocket 客户端（CONNECT/SUBSCRIBE/PUBLISH/PINGREQ） | 不引入完整 MQTT 栈：约 300 行，复用已有的 rustls 依赖 |
| `vr-meet::stun` | `Binding Request` → `XOR-MAPPED-ADDRESS` | 与打洞共用同一个套接字，保证发现的映射就是之后 QUIC 使用的映射 |
| `vr-meet::Announce` | 由联机码派生主题与 AES 密钥，发布/订阅候选地址 | 码不上线：主题是 `SHA-256("velarouter-meet-v1|" + 码)` 的前 15 字节 |
| `vr-agent::meet` | 接收侧：绑定固定 UDP 端口 → 宣布 → 打洞 → 交棒 QUIC | 会话结束后立刻重新开始，不因一次失败让机器“失联” |
| `vr-client::connect_meet` | 发起侧：并行拨号所有候选，首个成功者胜出 | 最多两次尝试，避免与会话结束的瞬间擦肩 |

设计取舍：

1. **不做中继**。中继意味着带宽成本与“公司能看到画面”的信任问题。代价是对称 NAT 之间无法直连，
   这一点在 UI 与文档里直说，而不是静默降级。
2. **会合服务可替换**。公共 MQTT 只是默认值；`--broker` / 设置页 / `VELAROUTER_BROKERS`
   都能指向自建 mosquitto。
3. **码是临时凭据**。40 bit 的码用来找到对方，密码用来证明身份，两件事分开。
4. **QUIC 的服务端套接字就是打洞用的套接字**。换套接字会换端口、换 NAT 映射，打洞就白做了。
5. **被控端只接受一个会话**。第二个会合请求会在 VRP 之前排队等待下一轮，UI 提示“对方没有响应”。
