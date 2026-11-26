# 用开源项目补齐已知限制

> 目标：把 `docs/ROADMAP.md` 里“还没做”的每一项，变成**可执行的选型 + 设计 + 代码骨架 + 验收标准**。
> 原则：优先成熟、许可宽松（MIT/Apache/BSD）、可交叉编译、无强制系统依赖的库；凡是需要 C 工具链的，都给出降级路径。

---

## 0. 选型原则

| 原则 | 含义 |
|---|---|
| 许可 | MIT / Apache-2.0 / BSD 优先；GPL 仅限独立进程且可替换 |
| 可交叉编译 | 尽量纯 Rust；C/C++ 依赖必须能在 CI 里构建（或提供 feature 开关） |
| 可降级 | 任何加速/增强都不能成为“唯一路径” |
| 协议先行 | 新能力先写进 `docs/PROTOCOL.md`，再写实现 |
| 可验证 | 每项都有可量化的验收指标，不写“做完了” |

---

## 1. UDP 媒体 + FEC + NACK

**现状**：媒体走 WebSocket（TCP），公网经中继时延迟受队头阻塞影响。`vr-transport` 已有
`FragmentHeader`（frame_id/index/count/total_len/nonce）与 `Reassembler`，但没有发送调度。

### 1.1 选型

| 方案 | 库 | 评价 |
|---|---|---|
| **QUIC 数据报** | [`quinn`](https://crates.io/crates/quinn) | 推荐。内置拥塞控制、丢包恢复、0-RTT、TLS；`send_datagram` 天然适配帧 |
| 裸 UDP + 自研 | `tokio::net::UdpSocket` | 已有原语；需要自己写拥塞控制，工作量大 |
| 完整 WebRTC | [`str0m`](https://crates.io/crates/str0m) / `webrtc-rs` | 能力最全，但会把项目拖回 UUYC 的复杂度 |
| FEC | [`reed-solomon-erasure`](https://crates.io/crates/reed-solomon-erasure) | 每 N 包 2 冗余，抗随机丢包 |
| 拥塞/带宽估计 | `quinn` 内置（BBR/Cubic） | 避免自研 |

**推荐组合**：`quinn` 作为**媒体传输**（不是替换控制面），控制面继续 WebSocket。
理由是：控制消息量小、要求可靠有序；媒体要求低延迟、可丢包。两者用不同传输是合理的。

### 1.2 协议扩展

```text
Control::Transport {
  udp: Some("203.0.113.7:51820"),   // QUIC 端点
  token: "<base64 16B>",            // 绑定会话，防串流
}
```

媒体帧继续用 `VideoHeader`，但放进 QUIC datagram 而不是 WebSocket：

```
datagram = session_token(8) || frame_seq(4) || VideoHeader(24) || payload
```

- `session_token` 由 `Transport.token` 派生，接收端校验；
- QUIC 已加密，**不再套 AES-GCM**（避免双重加密）；但 `VideoHeader` 仍然校验；
- 关键帧走可靠 stream（QUIC stream 0），P 帧走 datagram；
- 丢帧策略：解码端最多 1 帧在飞，缺参考帧时请求 IDR。

### 1.3 代码骨架

```rust
// crates/vr-net/src/quic.rs
use quinn::{Endpoint, Connection, RecvStream, SendStream};

pub struct MediaQuic {
    conn: Connection,
}

impl MediaQuic {
    pub async fn connect(addr: SocketAddr, server_name: &str, token: [u8; 8]) -> anyhow::Result<Self> {
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
        let conn = endpoint.connect(addr, server_name)?.await?;
        Ok(Self { conn })
    }

    pub async fn send_frame(&self, seq: u32, header: &[u8], payload: &[u8]) -> anyhow::Result<()> {
        let mut buf = Vec::with_capacity(12 + header.len() + payload.len());
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.extend_from_slice(header);
        buf.extend_from_slice(payload);
        self.conn.send_datagram(buf.into())?;
        Ok(())
    }
}
```

Agent 侧：`start_video_capture()` 产出帧后同时喂给 `SessionSender`（控制/兼容）与 `MediaQuic`（低延迟）；
由 `Control::VideoConfig` 的 `transport` 字段决定走哪条。

### 1.4 验收

- 20% 随机丢包、80ms RTT 下，1080p30 仍然连续（允许花屏但 1 秒内恢复）；
- 端到端延迟比 WebSocket 中继降低 ≥ 30%；
- 关闭 QUIC（feature off）时行为与现在完全一致。

**工作量**：8–12 人日（含 TLS 证书、打洞对接、测试）。

---

## 2. 硬件编码与 H.264

**现状**：只有 JPEG。1600×900 q60 实测 capture 30 ms + encode 47 ms，上限约 13 fps（release，M 系列 Mac）。

### 2.1 选型矩阵

| 平台 | 首选 | 备选 | Rust 接入 |
|---|---|---|---|
| macOS | VideoToolbox | openh264 软编 | `objc2` + `objc2-video-toolbox`（纯 Rust 绑定） |
| Windows | Media Foundation | NVENC/QSV/AMF | `windows` crate（MF）或 `ffmpeg-next` |
| Linux | VAAPI | NVENC | `ffmpeg-next` 或 `cros-libva` |
| Android | MediaCodec | openh264 | JNI / Kotlin 模块 |
| iOS | VideoToolbox | — | Swift（ReplayKit 扩展内） |
| 通用软编 | **openh264** | x264（GPL） | [`openh264`](https://crates.io/crates/openh264) crate（BSD，源码内置） |

**推荐**：`openh264` 作为统一软编基线（替换 JPEG 作为默认 H.264），硬件编码按平台 feature 接入。
openh264 是 Cisco 的 BSD 实现，`openh264` crate 自带源码，`cmake` 构建；CI 需要 cmake/nasm。

### 2.2 集成点

`FrameSource` 已经抽象了采集+编码，新增：

```rust
pub struct H264Capturer {
    inner: ScreenCapturer,          // 复用采集
    encoder: openh264::encoder::Encoder,
}

impl FrameSource for H264Capturer {
    fn next_frame(&mut self) -> Result<Option<EncodedFrame>> {
        let Some(raw) = self.inner.capture_raw()? else { return Ok(None) };
        let yuv = to_i420(&raw);
        let bitstream = self.encoder.encode(&yuv)?;
        Ok(Some(EncodedFrame { codec: Codec::H264, annexb: bitstream, .. }))
    }
}
```

`EncodedFrame` 增加 `codec: Codec`，`SessionSender::send_media` 已按 codec 写头。

### 2.3 客户端解码

- 浏览器：`WebCodecs.VideoDecoder`（`avc1.42E01E`），需要把 Annex-B 转成 `EncodedVideoChunk`；
  SPS/PPS 在第一个关键帧前注入。探测失败回落 JPEG。
- 原生：macOS `AVSampleBufferDisplayLayer`、Windows MF、Android `MediaCodec`。

### 2.4 验收

- 1080p30 H.264 码率 ≤ 6 Mbps，端到端延迟不高于 JPEG；
- CPU 占用比 JPEG 下降 ≥ 50%；
- 浏览器 WebCodecs 与原生解码器都能解；
- 无硬件编码时自动回退 openh264，再回退 JPEG。

**工作量**：15–20 人日（软编 + 三端硬编）。

---

## 3. 音频

**现状**：协议有 `Capabilities.audio` 与 `Transport`，无采集/编码/播放。

### 3.1 选型

| 环节 | 库 | 说明 |
|---|---|---|
| 采集/播放 | [`cpal`](https://crates.io/crates/cpal) | 跨平台，WASAPI/CoreAudio/ALSA/PulseAudio |
| 编码/解码 | [`audiopus`](https://crates.io/crates/audiopus) 或 `opus` | libopus 绑定；48 kHz、20 ms 帧、64 kbps |
| 回声消除 | [`webrtc-audio-processing`](https://crates.io/crates/webrtc-audio-processing) | 可选，会议场景 |
| 重采样 | [`rubato`](https://crates.io/crates/rubato) | 设备采样率不一致时 |
| 浏览器播放 | WebAudio `AudioWorklet` + `opus-decoder` (WASM) | 或直接用 `<audio>` + MediaStream（需要 WebRTC，不推荐） |

### 3.2 协议

```json
{"t":"audio_config","codec":"opus","sample_rate":48000,"channels":2,"bitrate":64000}
```

音频帧使用新的 envelope kind：

```
kind 4: AudioHeader(16B) || opus_payload
AudioHeader: magic "VRA1" | seq u32 | ts u32 | len u32
```

与视频共用会话密钥，但**独立计数器**（音频丢一帧不应该让视频解密失败）。
在 WebSocket 上按 20 ms 一帧发送，约 160 字节/帧，开销可忽略。

### 3.3 验收

- 本机回环延迟 < 60 ms；
- 48 kHz 立体声，64 kbps，主观无明显断续；
- 拔掉耳机/切换设备不崩溃（`cpal` 设备变更处理）；
- 一键静音（`mute`）。

**工作量**：6–8 人日。

---

## 4. 文件传输增强 + FUSE

**现状**：✅ 上传/下载/列目录/续传/校验已完成（`vr-agent/src/transfer.rs`、`velarouter get|put|ls`）。
剩余：

| 项 | 选型 | 工作量 |
|---|---|---|
| 目录递归上传/下载 | 自研遍历 + 并发 2–4 个传输 | 2 人日 |
| 断点续传 UI/CLI 参数 | 已有 `offset`，补 `--continue` | 0.5 人日 |
| 传输队列/限速 | `tokio::sync::Semaphore` + 令牌桶 | 2 人日 |
| 剪贴板文件（FUSE） | [`fuser`](https://crates.io/crates/fuser)（Linux/macOS）；Windows 用 WinFsp（C） | 8–12 人日 |
| 压缩传输 | `zstd` crate（可选） | 1 人日 |
| 断点元数据持久化 | SQLite（`rusqlite`）或 JSON sidecar | 2 人日 |

FUSE 设计要点：

```
远端路径 → 本地挂载点（只读优先）
read(offset, size) → 触发 FilePull(offset)，按需拉取
release() → 清理缓存块
```

只读模式先做（复制粘贴不需要写远端）；写入模式需要处理冲突与原子性，风险高，放后。

**验收**：复制一个 10 GB 目录到本地，只读取实际用到的文件；卸载后无残留进程。

---

## 5. 终端

**现状**：协议没有终端消息。

### 5.1 选型

| 环节 | 库 | 说明 |
|---|---|---|
| PTY | [`portable-pty`](https://crates.io/crates/portable-pty) | WezTerm 的跨平台 PTY（ConPTY/Unix98） |
| 终端状态机 | [`vt100`](https://crates.io/crates/vt100) 或 `alacritty_terminal` | 解析 ANSI，生成屏幕 |
| 渲染 | xterm.js（Web）/ 原生终端控件 | 与协议无关 |
| 会话复用 | 外部 `tmux`（可选） | 断线重连不丢 shell |

**核心决定**：终端只传 **PTY 原始字节流**，不传屏幕模型。这样客户端可以用任何终端模拟器，
和 SSH 一样简单。

### 5.2 协议

```json
{"t":"term_open","session":"work","cols":120,"rows":32,"shell":"/bin/zsh"}
{"t":"term_data","session":"work","data":"<base64>"}   // 或 envelope kind 5 二进制
{"t":"term_resize","session":"work","cols":140,"rows":40}
{"t":"term_close","session":"work"}
{"t":"term_list","sessions":[{"name":"work","cols":120,"rows":32,"created":1730000000}]}
```

二进制帧（kind 5）头：

```
magic "VRT1" | session_id u32 | len u32 | data
```

Agent 端：

```rust
let mut pty = portable_pty::native_pty_system()
    .openpty(PtySize { rows, cols, ..Default::default() })?;
let mut child = pty.slave.spawn_command(CommandBuilder::new(shell))?;
let mut reader = pty.master.try_clone_reader()?;
tokio::spawn(async move {
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        tx.send(Envelope::Term { session, data: buf[..n].to_vec() }).await?;
    }
});
```

**会话持久化**：Agent 侧维护 `HashMap<session, PtySession>`，客户端断开后 PTY 继续运行
（输出进环形缓冲，重连时重放最近的 N KB），这就是 tmux 的核心价值，不需要真的跑 tmux。

### 5.3 验收

- `vim`、`htop`、`tmux` 在远端正常渲染（含 256 色、鼠标、窗口大小变化）；
- 断线 60 秒后重连，shell 仍在；
- 中文输入与宽字符正确；
- Windows 走 ConPTY，无乱码。

**工作量**：10–14 人日（含 Web 端 xterm.js 与移动端键盘）。

---

## 6. 多显示器并发

**现状**：列表/切换已实现（Web UI 下拉，`video_config.display`），但同一时刻只推一路。

**目标**：一个会话同时推 N 路，客户端分屏或标签页。

设计：

- `VideoHeader.display` 已是每帧字段，无需改协议；
- Agent 为每个显示器维护独立编码器与帧通道，`session_id` 不变；
- `VideoConfig` 增加 `displays: [1,2]`（可选），空表示当前一路；
- 带宽分配：按焦点/鼠标所在屏给高帧率，其余降为 5 fps / 低质量；
- 客户端：Canvas 拼贴或标签页，`display` 过滤。

验收：双 4K 屏，主屏 30 fps，副屏 5 fps，总带宽 ≤ 12 Mbps。

**工作量**：4–6 人日。

---

## 7. JPEG 性能（libjpeg-turbo）

**现状**：`image` crate 的 JPEG 编码，1600×900 q60 约 47 ms/帧（实测）。

**方案**：[`turbojpeg`](https://crates.io/crates/turbojpeg)（绑定 libjpeg-turbo，UUYC 同款）。

```toml
[dependencies]
turbojpeg = { version = "1", optional = true }

[features]
default = ["turbo"]
turbo = ["dep:turbojpeg"]
```

```rust
let mut comp = turbojpeg::Compressor::new()?;
comp.set_quality(quality as i32)?;
comp.set_subsamp(turbojpeg::Subsamp::Sub2x2)?;
let image = turbojpeg::Image {
    pixels: rgb.as_raw().as_slice(),
    width: w, height: h, pitch: w * 3,
    format: turbojpeg::PixelFormat::RGB,
};
let jpeg = comp.compress_to_vec(image)?;
```

构建要求：`cmake`、`nasm`（x86 SIMD）。没有时自动回退 `image`。

预期：编码时间 47 ms → 10–15 ms，1080p30 可稳定。

验收：同分辨率/质量下 PSNR 差异 < 0.5 dB，编码时间下降 ≥ 50%。

**工作量**：1–2 人日。

---

## 8. 隐私屏

**需求**：被控端本地屏幕显示遮罩，但主控端仍能看到真实画面（UUYC `screen_privacy`）。

实现（不依赖额外库）：

1. 采集层：正常采集；
2. Agent 本地：创建置顶无边框窗口（`winit` + `softbuffer` 或平台 API），显示品牌遮罩；
3. 输入：遮罩窗口不拦截鼠标键盘（`set_ignore_cursor_events` / `WS_EX_TRANSPARENT`）；
4. 退出：快捷键或远程指令。

风险：全屏独占应用可能覆盖遮罩；需要提示用户使用“窗口化”或虚拟显示器。

**工作量**：3–5 人日（三平台）。

---

## 9. 虚拟按键 / 宏 / 手柄

- 纯客户端能力：把按键序列映射成 `InputEvent`，协议不变；
- 宏：`Vec<(InputEvent, delay_ms)>`，客户端定时发送；
- 手柄：`gilrs` crate（跨平台）读取手柄，映射为键盘/鼠标事件；
- 分享：JSON 方案文件，可选云同步（不属于核心）。

**工作量**：客户端 5–8 人日/端。

---

## 10. 服务化、更新与运维

| 项 | 选型 | 说明 |
|---|---|---|
| Windows 服务 | [`windows-service`](https://crates.io/crates/windows-service) | 服务 + 用户态采集进程配合 |
| macOS 自启 | `LaunchAgent`（`launchd` plist） | 文档已给模板 |
| Linux 自启 | systemd user unit | 文档已给模板 |
| 自动更新 | `self_update`（Rust）/ Sparkle（macOS）/ NSIS（Windows） | 签名校验必须 |
| 崩溃上报 | `sentry`（可选、默认关闭） | 隐私优先 |
| 结构化日志 | `tracing-subscriber` json feature | 已具备 |

---

## 11. 安全与配额

| 项 | 选型 | 说明 |
|---|---|---|
| 速率限制 | [`tower-governor`](https://crates.io/crates/tower-governor) | 每 IP 新建会话/连接数 |
| 设备身份 TOFU | `ed25519-dalek` | agent 长期公钥，客户端首次记录指纹 |
| TLS | `rustls` + `rcgen`（自签）或反代 | 已有 `reqwest/rustls`，可直接复用 |
| 审计 | `tracing` + 文件 sink | 不记录内容，只记录元数据 |
| 配额 | `Semaphore` + 每设备并发上限 | 防止单设备拖垮中继 |

---

## 12. 依赖与构建矩阵

| 依赖 | 引入项 | 需要系统工具 | 可选? |
|---|---|---|---|
| `quinn` | UDP/QUIC 媒体 | 无（rustls） | ✅ feature |
| `reed-solomon-erasure` | FEC | 无 | ✅ |
| `openh264` | H.264 软编 | cmake | ✅ |
| `turbojpeg` | JPEG 加速 | cmake + nasm | ✅ |
| `cpal` + `audiopus` | 音频 | ALSA 头（Linux） | ✅ |
| `portable-pty` | 终端 | 无 | ✅ |
| `fuser` | FUSE | libfuse（Linux/macOS） | ✅ |
| `gilrs` | 手柄 | 无 | 客户端 |

CI 建议：默认构建不含任何 C 依赖；`--features full` 在单独 job 里构建并跑互操作测试。

---

## 13. 里程碑与验收

| 里程碑 | 内容 | 人日 | 验收 |
|---|---|---|---|
| M1 | turbojpeg + 多显示器并发 + 文件传输增强 | 8 | 1080p30 稳定；双屏切换无感；10 GB 传输校验通过 |
| M2 | 终端 + WOL 辅助节点 | 14 | vim/htop 可用；断线重连；局域网唤醒成功 |
| M3 | openh264 + WebCodecs 解码 | 20 | 1080p30 ≤ 6 Mbps；CPU 降 50% |
| M4 | QUIC 媒体 + FEC | 12 | 20% 丢包可用；延迟降 30% |
| M5 | 音频 + 隐私屏 | 12 | 回环 < 60 ms；遮罩不拦截输入 |
| M6 | FUSE + TOFU + 配额 | 16 | 按需读取；首次信任；限速生效 |

总计约 **82 人日**。每一项都可独立交付，不阻塞其他项。

---

## 14. GitHub 稳定版选型（2026-11-26 核对）

按上游非预发布版本、当前传输兼容性、系统依赖和迁移成本取舍，不以更新时间或星数代替稳定性测试。

| 版本与上游 | 决策 | 验收边界 |
|---|---|---|
| [Caddy 2.11.9](https://github.com/caddyserver/caddy/releases/tag/v2.11.9) | **加入部署**：自动 TLS 和 WebSocket 反代，不让客户端手动维护证书 | 真实域名签证需要可达的公网服务器 |
| [Axum 0.8.9](https://github.com/tokio-rs/axum/releases/tag/axum-v0.8.9) / [rustls 0.23.46](https://github.com/rustls/rustls/releases/tag/v/0.23.46) | **沿用**既有 HTTP/WS 与 TLS，不换消息格式；reqwest 与 Quinn 统一使用 AWS-LC provider | 认证、健康检查、加密握手与回归测试 |
| [Quinn / quinn-proto 0.11.19](https://github.com/quinn-rs/quinn/releases/tag/quinn-proto-0.11.19) | **沿用** QUIC 直连（quinn 0.11.12）；保留 WSS 中继，不把 QUIC 误称为已支持 WebTransport | NAT、UDP 受限网络仍需实网验证 |
| [egui 0.37.0](https://github.com/emilk/egui/releases/tag/0.37.0) | **已迁移**桌面 UI 生命周期、原生拖放与输入法事件 | 桌面跨平台实机验收另行进行 |
| [webrtc-rs 0.17.3](https://github.com/webrtc-rs/webrtc/releases/tag/v0.17.3) | **候选，未接入**：浏览器实时媒体可试点 ICE/DTLS/SRTP；不能直接替代 VRP/1 文件、输入与认证协议 | 独立原型对比首帧、丢包恢复、TURN 成本、端到端安全后再决定 |

本轮锁文件审计：`cargo audit` 未报告已知漏洞，但有 4 个间接依赖的停止维护提示（`ttf-parser`、`zbus_names`、`zvariant`、`zvariant_derive`）。等待 UI / Linux 桌面栈上游迁移，不强行覆盖不兼容的依赖约束。复核方式：`cargo audit`、`(cd webviewer && npm ci && npm audit && npm test)`、`(cd sdk/go && go mod verify && go test ./...)`。

### 相关参考实现

桌面端已采用的两项做法（egui 0.37）：

- 输入法：参考 [egui 组词显示讨论](https://github.com/emilk/egui/pull/8083) 并核对 [0.37 原生桥接代码](https://github.com/emilk/egui/blob/0.37.0/crates/egui-winit/src/lib.rs)。预编辑仅在主控端绘制；将小矩形而非整张视频交给系统定位候选窗。候选列表仍由本机输入法提供。
- 目录浏览：采用 [ScrollArea::show_rows](https://github.com/emilk/egui/blob/0.37.0/crates/egui/src/containers/scroll_area.rs) 的固定行高布局，只构建可视范围的文件行。此优化针对绘制，初次枚举目录仍需要读取目录项。

| 领域 | 项目 | 可借鉴点 |
|---|---|---|
| 远程桌面协议 | [RustDesk](https://github.com/rustdesk/rustdesk) | 采集/编码/输入的平台适配、中继 |
| Android 被控 | [scrcpy](https://github.com/Genymobile/scrcpy) | MediaCodec + `InputManager` 注入（shell 权限） |
| 终端 | [WezTerm](https://github.com/wez/wezterm) | `portable-pty` 的来源与用法 |
| QUIC | [quinn](https://github.com/quinn-rs/quinn) | datagram 与 stream 的混合使用 |
| 编码 | [openh264-rs](https://github.com/ralfbiedert/openh264-rs) | 软编封装 |
| JPEG | [libjpeg-turbo](https://github.com/libjpeg-turbo/libjpeg-turbo) | UUYC 也在用 |
| 音频 | [cpal](https://github.com/RustAudio/cpal) | 设备热插拔处理 |
| FUSE | [fuser](https://github.com/cberner/fuser) | 用户态文件系统 |
| 手柄 | [gilrs](https://gitlab.com/gilrs-project/gilrs) | 跨平台输入映射 |

> 借鉴代码前先核对许可证；GPL 项目（如部分 x264 构建）不要静态链入 Apache-2.0 产物。
