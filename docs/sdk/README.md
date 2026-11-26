# SDK 指南

Velarouter 的协议只有一层（VRP/1），因此“SDK”不是黑盒，而是**同一套步骤的三种语言实现**。
选择最接近你目标平台的那一个，其余当作交叉验证。

| 语言 | 位置 | 用途 | 验证程度 |
|---|---|---|---|
| Rust | `crates/vr-session`、`crates/vr-agent` | 被控端、CLI、原生客户端 | 生产实现，38 个单测 |
| JavaScript | `webviewer/app.js` | 浏览器 / WebView 主控端 | 与 Rust 互通（实测） |
| Go | `sdk/go` | MyGo 客户端、服务端工具 | 已知答案向量 + 真实连接（实测） |

---

## 1. 三者的共同步骤

无论哪种语言，客户端必须按顺序完成：

```
1. 生成 8 字节 session_id
2. 连接 ws://host:34789/vr?sid=<session_id>
3. 发送 Hello（文本）
4. 生成临时 P-256 密钥对
5. 发送 AuthInit（viewer_pub + nonce）
6. 收到 AuthFinish → 计算 ECDH → HKDF → 校验 agent proof
7. 发送 AuthProof（viewer proof）
8. 收到 AuthOk
9. 发送 Caps（第一条密文）
10. 收到 Caps → 拿到 agent 能力
11. 发送 VideoConfig
12. 循环：解密 → kind=1 控制 / kind=2 媒体
```

任何一步顺序错误都会失败；不要“优化”成并行。

---

## 2. Rust

```rust
use vr_session::{Session, SessionConfig, WsChannel};
use vr_transport::MessageChannel;
use vr_proto::{Capabilities, Control, VideoConfig};

let channel = WsChannel::connect("ws://192.168.1.10:34789/vr").await?;
let session = Session::connect(
    Box::new(channel) as Box<dyn MessageChannel>,
    SessionConfig {
        session_id: "s1".into(),
        secret: "123456".into(),
        app: "my-client/0.1".into(),
        device: None,
        capabilities: Capabilities::default(),
    },
).await?;

let (mut tx, mut rx, peer) = session.into_parts();
tx.send_control(Control::VideoConfig(VideoConfig::default())).await?;
while let Ok(ev) = rx.recv().await {
    if let vr_session::Event::Media { header, payload } = ev {
        // decode payload
    }
}
```

Rust 的优势：可以直接复用 `vr-agent` 的采集与注入，做成“同城内网中转节点”。

---

## 3. JavaScript

`webviewer/app.js` 里的 `VrClient` 类就是完整的 JS SDK，无依赖：

```js
const client = new VrClient({
  code: "123456",
  sessionId: "s1",
  onFrame: ({ bitmap, width, height }) => {
    canvas.width = width; canvas.height = height;
    canvas.getContext("2d").drawImage(bitmap, 0, 0);
  },
  onControl: (msg) => { /* stats / display_list / warning */ },
});
await client.connect();
client.sendControl({ t: "video_config", codec: "jpeg", quality: 70,
                     fps: 30, max_width: 1600, max_height: 900, display: 0 });
```

浏览器安全上下文：`crypto.subtle` 只在 `https://` 或 `localhost` 下可用。
局域网 `http://192.168.x.x` 不是安全上下文，因此 viewer **自带一个 51 KB 的
纯 JS 兜底实现**（`webviewer/vendor/noble.js`：P-256 / HKDF / HMAC / AES-GCM），
在检测不到 WebCrypto 时自动启用。可用 `?fallback=1` 强制走兜底路径做测试。

两条路径都通过了与 Rust agent 的真实互通验证，性能差异在本机测试中 < 1 ms/帧。

---

## 4. Go

```go
client, err := velarouter.Dial(ctx, "ws://192.168.1.10:34789/vr", "123456",
    velarouter.Options{App: "my-mygo-app/0.1"})
defer client.Close()

if err := client.RequestVideo(1600, 900, 70, 30); err != nil { return err }

for {
    frame, err := client.Next(ctx, func(c velarouter.Control) {
        if s, ok := velarouter.StatsFrom(c); ok {
            log.Printf("%.1f fps, %d kbps", s.FPS, s.Kbps)
        }
    })
    if err != nil { return err }
    // frame.Payload 是 JPEG 字节，可直接交给 WebView
}
```

在 MyGo 中把 `frame.Payload` 通过 `mygo.Event` / `Channel[[]byte]` 推给前端，
前端用 `createImageBitmap` 渲染。详见 [`../MYGO.md`](../MYGO.md)。

---

## 5. 一致性测试

发布前至少跑：

```bash
# Rust
cargo test
cargo run -p vr-crypto --example vector

# Go
cd sdk/go && go test ./...

# 端到端
./target/release/velarouter agent --code 654321 &
./target/release/velarouter probe --url ws://127.0.0.1:34789/vr --code 654321
cd sdk/go && go run ./cmd/smoke -code 654321
```

三者的 `session_key` 与两个 `proof` 必须完全一致（见 PROTOCOL §3.4）。

---

## 6. 错误处理约定

| 阶段 | 错误 | 客户端动作 |
|---|---|---|
| 连接 | TCP/WS 失败 | 换 lan_hint，再回落中继 |
| 握手 | `auth_fail` | 提示配对码错误，不要重试同一码 |
| 解密 | counter 不连续 | 立即断开，不尝试恢复 |
| 媒体 | 头长度不符 | 丢弃该帧，连续 3 次则断开 |
| 控制 | 未知 `t` | 忽略并记录 |
| 网络 | 60 秒无数据 | 主动 ping，两次无响应则重连 |

---

## 7. 性能建议

- 解码不要排队：最多 1 个 decode 在飞；
- JPEG 用 `createImageBitmap`（浏览器）/ `BitmapFactory`（Android），不要 `Image` 逐像素；
- 帧缓冲复用，避免每秒分配 30 个 100KB 的 `Vec`；
- 输入事件合并：鼠标移动 60Hz 足够，不要每个 `mousemove` 都发；
- 统计信息每秒一次，不要每帧更新 UI。
