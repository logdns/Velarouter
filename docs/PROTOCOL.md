# VRP/1 协议规范

> 版本：1（`PROTOCOL_VERSION = 1`）
> 状态：稳定，已实现于 Rust 与 JavaScript，并通过互通测试。
> 目标读者：要实现 Velarouter 客户端或服务端的工程师。

本文与代码一一对应：

| 内容 | 代码 |
|---|---|
| 消息与帧 | `crates/vr-proto/src/lib.rs` |
| 握手与信封 | `crates/vr-session/src/lib.rs` |
| 密钥与加解密 | `crates/vr-crypto/src/lib.rs` |
| 分片 | `crates/vr-transport/src/lib.rs` |
| 中继前缀 | `crates/vr-signal/src/lib.rs`、`crates/vr-session/src/lib.rs` |
| JSON 样例 | `cargo run -p vr-proto --example dump_json` |
| 测试向量 | `cargo run -p vr-crypto --example vector` |

---

## 1. 总体

VRP/1 有两层，共用一条双向消息通道：

```
┌────────────────────────────────────────────┐
│ 控制面：UTF-8 JSON，一条消息一个消息帧         │
├────────────────────────────────────────────┤
│ 媒体面：24 字节头 + 原始 JPEG/H.26x 载荷      │
└────────────────────────────────────────────┘
```

握手阶段控制面是明文；`AuthOk` 之后，**所有**消息（控制 + 媒体）都装进同一个加密信封。

### 1.1 常量

| 名称 | 值 | 含义 |
|---|---|---|
| `PROTOCOL_VERSION` | `1` | 主版本，不兼容即拒绝 |
| `DEFAULT_AGENT_PORT` | `34789` | 被控端 TCP/WS 端口 |
| `DEFAULT_SIGNAL_PORT` | `34790` | 信令服务器 HTTP/WS 端口 |
| `DEFAULT_REFLEX_PORT` | `34791` | mini-STUN / 打洞 UDP 端口 |
| `MEDIA_MAGIC` | `"VRF1"` | 媒体帧魔数 |
| `VIDEO_HEADER_LEN` | `24` | 媒体头长度 |
| `MAX_CONTROL_BYTES` | `1 MiB` | 单条控制消息上限 |
| `MAX_MEDIA_PAYLOAD` | `8 MiB` | 单帧媒体上限 |
| `MAX_MESSAGE` | `16 MiB` | 流分帧上限 |
| `UDP_CHUNK` | `1200` | UDP 分片载荷 |

---

## 2. 传输绑定

同一条 VRP 会话可以跑在四种通道上，语义完全一致。

### 2.1 WebSocket（推荐）

- 一个文本帧 = 一条 JSON 控制消息（仅握手阶段）；
- 一个二进制帧 = 一个加密信封；
- 子协议、扩展都不协商；
- 服务端必须在 10 秒内完成握手，否则关闭。

### 2.2 流（TCP / Unix Socket）

```
[u32 big-endian length][payload]
```

`payload` 为一条 JSON 或一个加密信封。最大 16 MiB，超限直接断开。

### 2.3 中继（WebSocket 上的隧道）

中继既要转发 VRP，又要有自己的控制消息，因此二进制帧带 1 字节前缀：

| 前缀 | 含义 |
|---|---|
| `0x01` | 后面是 VRP 明文 JSON（握手） |
| `0x02` | 后面是 VRP 加密信封 |

中继控制消息是**文本帧**，形如 `{"t":"ready","lan_hints":[...]}`。
中继服务器不解密、不解析二进制帧。

### 2.4 UDP（媒体平面，v0.2）

每个数据报：

```
FragmentHeader (16 bytes) || sealed chunk
```

| 偏移 | 类型 | 字段 |
|---|---|---|
| 0 | u32 | frame_id |
| 4 | u16 | index |
| 6 | u16 | count |
| 8 | u32 | total_len |
| 12 | u32 | nonce |

UDP 没有顺序保证，因此不能用隐式计数器；nonce 由发送端显式给出，
接收端用它构造 AES-GCM IV（`direction || 0,0,0 || nonce`），允许丢包与乱序。

---

## 3. 握手

### 3.1 时序

```
Viewer                                      Agent
  │                                            │
  │ 1. Hello            (text, plaintext)  ──► │
  │ 2. AuthInit         (text, plaintext)  ──► │
  │                                            │ 3. ECDH + HKDF
  │ 4. AuthFinish       (text, plaintext)  ◄── │
  │ 5. 验证 agent proof                        │
  │ 6. AuthProof        (text, plaintext)  ──► │
  │                                            │ 7. 验证 viewer proof
  │ 8. AuthOk           (text, plaintext)  ◄── │
  │ ═══════════ 之后全部为二进制加密信封 ═══════ │
  │ 9. Caps             (sealed)           ──► │
  │ 10. Caps            (sealed)           ◄── │
```

### 3.2 消息（逐字对应实现）

```json
{"t":"hello","version":1,"role":"viewer","device":null,"session_id":"7f3c9a2b","app":"webviewer/0.1"}
```

| 字段 | 说明 |
|---|---|
| `version` | 必须是 `1` |
| `role` | viewer 发 `"viewer"`；agent 不会主动发 Hello |
| `device` | 可选 `DeviceInfo`，用于在被控端显示“谁在连接” |
| `session_id` | 本次连接的随机 ID，参与密钥派生 |
| `app` | 客户端名称/版本，仅用于日志 |

```json
{"t":"auth_init","viewer_pub":"<base64 SEC1 uncompressed>","nonce":"<base64 16 bytes>"}
```

`viewer_pub` 是 P-256 公钥的 **未压缩 SEC1** 编码（65 字节，`0x04 || X || Y`），base64 标准编码。
`nonce` 当前保留，用于未来扩展（例如 PAKE），SDK 应生成 16 字节随机数。

```json
{"t":"auth_finish","agent_pub":"<base64 SEC1 uncompressed>","proof":"<base64 HMAC-SHA256>"}
```

```json
{"t":"auth_proof","proof":"<base64 HMAC-SHA256>"}
{"t":"auth_ok"}
{"t":"auth_fail","reason":"pairing code mismatch"}
```

`agent_pub` 每次连接都必须是新生成的临时密钥（不允许复用长期密钥），否则会破坏前向保密。

### 3.3 密钥派生

```
shared = ECDH(my_private, peer_public)          // 32 bytes，x 坐标
salt   = UTF8(session_id) || 0x7C /* '|' */ || UTF8(pairing_code)
key    = HKDF-SHA256(ikm=shared, salt=salt,
                     info="velarouter/vrp1/session-key", L=32)
```

| 参数 | 值 |
|---|---|
| KDF | HKDF（RFC 5869），SHA-256 |
| salt | `session_id + "|" + pairing_code` |
| info | ASCII `velarouter/vrp1/session-key` |
| 输出 | 32 字节 |

证明：

```
proof(label) = base64(HMAC-SHA256(key, label || "|" || session_id))
agent  的 label = "agent"
viewer 的 label = "viewer"
```

比较必须使用常量时间比较。

### 3.3.1 可选的可信设备验证（开发版）

受限被控端在明文 `auth_finish` 中置 `require_viewer_identity: true`。主控在验证被控端的常规密码证明后，可在 `auth_proof` 中附 `viewer_identity: {public_key, signature}`。两字段均为标准 Base64：公钥为压缩 SEC1 P-256（33 字节），签名为 ECDSA P-256/SHA-256 的固定宽度 `r || s`（64 字节）。签名覆盖域标签 `velarouter/vrp1/trusted-viewer/v1`、一个 NUL、32 字节会话密钥和公钥的规范 Base64 ASCII，按此顺序拼接。会话密钥不发送。被控端先验证原密码证明，再检查公钥是否在本地名单并验签；任何一项失败都不会发送 `auth_ok` 或进入能力交换。

未启用限制时省略要求字段，主控不发送身份签名，密码握手和既有已知答案向量不变。旧主控只兼容未启用限制的被控端。配对字符串 `VR-PAIR1:<公钥>` 只用于编辑本地名单，不是新的联机码。此扩展只验证主控端身份。

### 3.4 已知答案测试向量

固定输入：

```
session_id   = "s1"
pairing_code = "123456"
viewer_priv  = 0x0000…0001
agent_priv   = 0x0000…0002
```

期望输出：

```
viewer_pub_b64  = BGsX0fLhLEJH+Lzm5WOkQPJ3A32BLeszoPShOUXYmMKWT+NC4v4af5uO5+tKfA+eFivOM1drMV7Oy7ZAaDe/UfU=
agent_pub_b64   = BHzyexiNA09+ilI4AwS1GsPAiWnid/IbNaYLSPxHZpl4B3dVENuO0EApPZrGn3Qw27p9reY86YIpngS3nSJ4c9E=
shared_secret   = 7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978
session_key     = 251d80ca3d587258fef045dad50e815efce55f634cad47f89a3a913be2b56238
proof_agent     = DgVzWu+hMaJiooLH+ErnGm8Kwqr52QHSHJLskvzikN0=
proof_viewer    = q+S7nllk/myg+t5yobiRHo2NqwwFxcjaG3IkZgPuJVQ=
```

`shared_secret` 与 NIST P-256 公开测试向量一致，可用于排查 ECDH 实现。

> 任何 SDK 的 CI 都应该包含这组断言。密钥或 proof 不一致，说明永远无法互通。

### 3.5 失败处理

| 情况 | 发送方 | 行为 |
|---|---|---|
| 版本不支持 | Agent | `auth_fail{reason:"unsupported version N"}`，关闭 |
| session_id 不匹配（显式指定时） | Agent | `auth_fail`，关闭 |
| agent proof 不匹配 | Viewer | `auth_fail{reason:"agent proof mismatch"}`，关闭 |
| viewer proof 不匹配 | Agent | `auth_fail`，关闭 |
| 10 秒未完成 | 双方 | 关闭 |

窗口不得重复尝试同一配对码；实现应在本地做失败计数与退避（建议 5 次后锁定 60 秒）。

---

## 4. 加密信封

握手完成后，每条消息：

```
plaintext  = kind:u8 || body
ciphertext = AES-256-GCM(key, iv, plaintext, aad="vrp1")
wire       = counter:u64 (big-endian) || ciphertext || tag(16)
```

| 字段 | 说明 |
|---|---|
| `kind` | `1` = JSON 控制消息，`2` = 媒体帧，`3` = 文件块 |
| `counter` | 每个方向独立，从 0 开始，每条消息 +1 |
| `iv` | 12 字节：`direction(1) || 0x00 0x00 0x00 || counter(8, BE)` |
| `direction` | viewer→agent = `1`，agent→viewer = `2` |
| `aad` | ASCII `vrp1` |
| tag | 16 字节，追加在密文后 |

规则：

1. 接收方必须校验 `counter == expected`，否则断开（不尝试重同步）。
2. 发送方必须保证 `counter` 不回退；进程重启后必须使用新密钥。
3. `kind` 未知时忽略该消息并记录，不要断开（前向兼容）。
4. 解密失败一律断开，不返回具体原因（避免 padding oracle 类攻击）。

---

## 5. 媒体帧

```
offset size field
0      4    magic       "VRF1"
4      1    version     1
5      1    codec       1=JPEG 2=H.264 3=H.265 4=VP8 5=AV1
6      1    flags       bit0 KEYFRAME, bit1 TILED
7      1    reserved    0
8      4    frame_id    自增
12     4    display     显示器 id
16     2    width       编码后宽
18     2    height      编码后高
20     4    payload_len 载荷字节数
24     N    payload
```

- 所有多字节字段 **大端**；
- `frame_id` 从 0 开始，每个会话自增；
- JPEG 帧必须置 `KEYFRAME`；
- `TILED` 表示载荷是多个瓦片拼接（v0.2，当前恒为 0）；
- 解码端应先读 `frame_meta`（可选）再读帧，二者 `frame_id` 对应。

### 5.1 媒体与控制共用计数器

媒体和控制共用 `kind` 信封，因此它们的顺序由同一个 `counter` 保证。
这消除了“视频到了但配置没到”的竞态。

### 5.2 丢帧策略

客户端不得排队等待解码：同一时刻最多一个 `decode` 在飞，超出的帧直接丢弃。
`Stats.dropped` 记录丢弃数。这是远程桌面在弱网下保持低延迟的关键。

---

## 5.3 文件块

文件传输使用独立的 envelope kind，避免把二进制塞进 JSON：

```
kind = 3
body = FileHeader(28) || payload
```

```
offset size field
0      4    magic       "VRFL"
4      1    version     1
5      1    flags       bit0 LAST（最后一个块）
6      2    reserved    0
8      4    transfer_id
12     8    offset      文件内字节偏移
20     4    seq         块序号（从 0 开始）
24     4    len         载荷长度
28     N    payload
```

- 块大小默认 64 KiB（`FILE_CHUNK`）；
- `offset` 必须等于接收端已写入的字节数，否则整个传输失败（防止空洞与乱序改写）；
- 一次传输的所有块共享 `transfer_id`，与控制的 `FilePull/FilePush` 对应；
- 传输结束后由控制消息给出 SHA-256，不依赖块级校验。

## 6. 控制消息目录

所有消息 `t` 为判别字段，未知 `t` 应忽略。

### 6.1 握手（明文）

| `t` | 方向 | 关键字段 |
|---|---|---|
| `hello` | V→A | `version, role, session_id, app, device?` |
| `auth_init` | V→A | `viewer_pub, nonce` |
| `auth_finish` | A→V | `agent_pub, proof` |
| `auth_proof` | V→A | `proof` |
| `auth_ok` | A→V | — |
| `auth_fail` | 双向 | `reason` |

### 6.2 会话配置

```json
{"t":"caps","capabilities":{"codecs":["jpeg"],"max_width":3840,"max_height":2160,
 "max_fps":60,"input":true,"clipboard":true,"multi_display":true,"audio":false},
 "device":{"device_id":"f2b1","name":"Studio","platform":"macos-aarch64","version":"0.1.0","tags":["agent"]}}
```

```json
{"t":"transport","udp":"203.0.113.7:51820","token":"t"}
```

`transport` 用于通告 UDP 媒体端点；`udp` 缺省表示“媒体走控制通道”。

### 6.3 显示与视频

```json
{"t":"display_list","displays":[{"id":1,"name":"Built-in","width":2560,"height":1440,"scale":2.0,"primary":true}]}
{"t":"video_config","codec":"jpeg","quality":70,"fps":30,"max_width":1920,"max_height":1080,"display":0}
{"t":"frame_meta","frame_id":42,"display":1,"width":1600,"height":900,"codec":"jpeg","keyframe":true,"tiles":1}
```

- `video_config` 由 Viewer 发送；Agent 可裁剪到自身与 `caps` 的较小值；
- `quality` 20..95，越界由 Agent clamp；
- 调整 `fps` 或画面尺寸上限会重建采集编码器；不修改操作系统显示模式。
- H.264 试验版要求两端的 `caps.codecs` 都含 `h264`，并由主控发送 `video_config.codec=h264` 后才启用。VRF1 帧负载为一个 Annex-B access unit，IDR 帧附 SPS/PPS 并设置 `KEYFRAME`。发送端丢失待发送帧后请求新的 IDR；接收端重置后等待 IDR。任一端不支持、还未协商或编码器出错时，仍可收到 JPEG；接收端应按每帧的 codec 字段解码。

#### 桌面墙只读会话

桌面墙使用现有 VRP/1 握手和认证，Viewer 的 `app` 以 `velarouter-wall/` 开头。支持该模式的 Agent 在认证后首先发加密控制消息 `{"t":"view_only"}`，然后才发显示器列表和画面；Viewer 没有收到此确认就退出，不降级。该会话仅传 JPEG，限制为 480×270 / 3 fps；Agent 不处理输入、剪贴板、文件或音频请求。该模式不是新的授权凭证，不改变密码和可信设备规则，也不将其他已获授权的普通会话限制为只读。

#### 可选系统音频

仅在双方 `caps.audio = true`、被控端允许且主控显式发送 `{"t":"audio_request","enabled":true}` 时启用。被控端先回 `audio_status`（`active` 与 `message`），成功后可发送加密信封 `kind=4`：其余 3840 字节为 20 ms 的 48 kHz、双声道交错、signed 16-bit little-endian PCM。每个单元独立；拥堵时可丢弃未发出的音频单元，不重传。`enabled:false` 停止，无需重新握手。关闭／采集失败后发送 `active:false`。旧版和不支持音频的客户端不发送请求，也不接收音频单元。

### 6.4 输入

```json
{"t":"input","event":{"k":"mouse_move","x":32768,"y":16384,"display":1}}
{"t":"input","event":{"k":"mouse_button","button":"left","down":true,"x":32768,"y":16384,"display":1}}
{"t":"input","event":{"k":"wheel","dx":0,"dy":-3,"x":32768,"y":16384,"display":1}}
{"t":"input","event":{"k":"key","code":"KeyA","down":true,"modifiers":1}}
{"t":"input","event":{"k":"text","text":"你好"}}
{"t":"input","event":{"k":"touch","phase":"move","points":[{"id":0,"x":100,"y":200}],"display":0}}
{"t":"input","event":{"k":"release_all"}}
```

坐标一律为 `0..=65535` 的归一化值：

```
x_norm = round(clamp(x_px / display_width, 0, 1) * 65535)
x_px   = (x_norm / 65535) * display_width
```

`code` 使用 W3C `KeyboardEvent.code`（`KeyA`、`Digit1`、`Enter`、`F11`、`ArrowLeft`…）。
`button` 取值 `left|middle|right|back|forward`。
`phase` 取值 `start|move|end|cancel`。
`modifiers` 位掩码：`1=Shift 2=Control 4=Alt 8=Meta 16=CapsLock 32=NumLock`。

> 为什么用 W3C code 而不是虚拟键码：浏览器可直接发送，Android 的 `KeyEvent` 与 iOS 的
> `UIKeyboardHIDUsage` 也都能映射；虚拟键码是 Windows 专有，跨端会丢布局信息。

### 6.5 剪贴板

```json
{"t":"clipboard","direction":"viewer_to_agent","text":"hello"}
```

接收方只接受与自己相反的方向（防回环）。文本超过 1 MiB 会被拒绝。

### 6.6 光标

```json
{"t":"cursor","visible":true,"shape":"default","x":100,"y":200}
```

`shape` 使用 CSS cursor 关键字（`default|text|pointer|crosshair|move|not-allowed|…`）。
当前 Agent 未发送此消息，协议已定义。

### 6.7 统计与心跳

```json
{"t":"stats","fps":30.0,"kbps":4200,"rtt_ms":8,"capture_ms":6.2,"encode_ms":11.4,
 "decode_ms":3.1,"dropped":0,"e2e_ms":24}
{"t":"ping","ts":1730000000000}
{"t":"pong","ts":1730000000000}
```

- 双方各每秒发一次 `stats`；
- 任一方可发 `ping`，收到后必须原样回 `ts`；
- RTT 由发送方计算（`now - ts`），只有发送方本地的时钟参与，无需时钟同步。

### 6.9 文件传输

```json
{"t":"file_list_request","request_id":1,"path":"Documents"}
{"t":"file_list","request_id":1,"path":"Documents","root":"/srv/shared","entries":[
  {"name":"a.txt","path":"Documents/a.txt","is_dir":false,"size":12,"mtime":1730000000,"mode":420}]}
{"t":"file_list_error","request_id":1,"code":"list","message":"no such file"}

{"t":"file_pull","transfer_id":7,"path":"Documents/a.txt","offset":0}
{"t":"file_push","transfer_id":8,"name":"report.pdf","size":8388608,"offset":0,"overwrite":true}
{"t":"file_progress","transfer_id":7,"offset":4194304,"total":8388608}
{"t":"file_complete","transfer_id":7,"path":"a.txt","sha256":"<64 hex>"}
{"t":"file_error","transfer_id":8,"code":"offset","message":"expected offset 65536, got 131072"}
{"t":"file_cancel","transfer_id":7}
```

规则：

1. `file_list.root` 是可选的共享目录显示路径，不用于请求。`path` 永远相对于被控端的共享根目录，禁止 `..`、绝对路径与逃逸符号链接；
2. `file_pull` 由主控端发起，被控端按 `offset` 续传；
3. `file_push` 先声明，随后发 `kind=3` 的块；最后一块置 `LAST`；
4. 接收端写临时文件（`.<name>.velarouter-part`），完成后 `rename`，再做 SHA-256 校验；
5. `file_progress` 最多每 4 MiB 一次，避免控制面被进度消息淹没；
6. `file_complete.path` 是**文件名**（不是绝对路径），避免泄露服务端目录结构；
7. 任一方向都可发 `file_cancel`；接收端必须删除临时文件。

### 6.10 生命周期

```json
{"t":"warning","code":"capture","message":"screen recording permission missing"}
{"t":"bye","reason":"viewer closed"}
{"t":"error","code":"protocol","message":"unexpected frame"}
```

| 消息 | 语义 |
|---|---|
| `warning` | 不中断会话的问题，UI 应展示 |
| `bye` | 优雅结束，接收方回 `bye` 后关闭 |
| `error` | 致命错误，接收方直接关闭 |

建议的 `code`：`capture`、`input`、`codec`、`video_config`、`protocol`、`auth`、`rate_limit`。

---

## 7. 状态机

```
                 ┌──────────┐
                 │  Idle    │
                 └────┬─────┘
        connect()     │
                 ┌────▼─────┐
                 │Handshake │◄──── auth_fail / timeout ──► Closed
                 └────┬─────┘
              auth_ok │
                 ┌────▼─────┐
                 │  Ready   │  caps 交换中
                 └────┬─────┘
             caps 完成 │
                 ┌────▼─────┐
             ┌──►│ Streaming│◄──┐
             │   └────┬─────┘   │
             │        │ bye/err │
             │   ┌────▼─────┐   │
             └───┤ Closing  ├───┘
                 └────┬─────┘
                 ┌────▼─────┐
                 │  Closed  │
                 └──────────┘
```

`Streaming` 状态下允许随时重发 `video_config`；不允许重新握手。

---

## 8. 版本演进规则

1. **主版本**（`version`）任何不兼容变更递增，旧端必须拒绝。
2. **新增 `t`** 不需要升版本，旧端忽略。
3. **新增可选字段** 不需要升版本，旧端用默认值。
4. **修改字段语义/删除字段** 必须升版本。
5. 媒体头 `version` 独立于控制面，但当前与主版本保持一致。

---

## 9. 一致性检查清单（SDK 发布前）

- [ ] 通过 §3.4 的已知答案向量（密钥 + 两个 proof）
- [ ] 能拒绝错误配对码，且不泄露“错在哪一步”
- [ ] 能拒绝重放的密文（计数器校验）
- [ ] 能正确处理 `server push` 的 `ping`，不把它暴露给 UI
- [ ] 媒体帧 `payload_len` 与实际长度不符时安全失败
- [ ] 未知 `t` 被忽略而不是断开
- [ ] 断开后清空会话密钥与计数器
- [ ] 客户端同一时刻最多一个解码任务在飞
- [ ] 坐标在分辨率变化后仍正确（归一化）
- [ ] 60 秒无数据时主动 `ping`，避免中间设备回收连接
- [ ] 文件块 `offset` 不连续时安全失败，不写坏目标文件
- [ ] 上传完成前不覆盖同名文件（临时文件 + 原子 rename）
- [ ] 路径穿越（`..`、绝对路径、符号链接）被拒绝

---

## 10. 附：生成 JSON 样例

```bash
cargo run -p vr-proto --example dump_json
cargo run -p vr-crypto --example vector
```

两段输出分别对应本文 §6 与 §3.4，可作为文档漂移检测。
