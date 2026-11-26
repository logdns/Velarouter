# 可行性分析：用 MyGo 做 Velarouter 客户端

> 结论先说：**可以，而且适合做主控端外壳；但不适合做被控端核心，也不该替换协议与媒体层。**
> 本文给出实测证据、架构方案、工作量与风险，最后是 go / no-go 判定表。

分析对象：`https://mygo.run/zh/docs/getting-started`（MyGo 0.10.0，MIT）。
验证环境：macOS 26.2 / Apple Silicon，Go 1.27.4，Xcode 27，Android SDK 37。

---

## 1. MyGo 是什么

一句话：**用 Go 写桌面/移动应用外壳，窗口显示系统 WebView 或 MyGo 自绘的原生 UI。**

关键事实（来自官方文档 + 本机实测）：

| 维度 | 事实 |
|---|---|
| 语言 | Go（业务/窗口/网络）+ TypeScript（Web 前端）或纯 Go（原生 UI） |
| 桌面 | WKWebView（macOS）、WebView2（Windows）、WebKitGTK 4.1（Linux） |
| 桌面原生 UI | Metal / Direct3D 11 / OpenGL / CPU 自绘，系统文本引擎 |
| 桌面编译 | **全程 `CGO_ENABLED=0`**，任意平台交叉编译 |
| 原生互操作 | `purego`（dlopen + 汇编 trampoline，macOS/Linux）、`syscall`（Windows） |
| 移动 | iOS（WKWebView + Metal）、Android（WebView + OpenGL ES）；用各自工具链 + cgo |
| 原生模块 | Swift / Kotlin / Rust / Zig 实现 Go 接口；**标注为实验性** |
| 插件 | fetch、websocket、sqlite、watch、updater、terminal、glass、game、loading、SF Symbols、automation、PermissionFlow、render |
| 打包 | macOS `.app` + `.dmg`；Windows `.exe` + NSIS 安装器；Linux 可执行 + `.deb`/`.rpm`/`install.sh`；iOS `.ipa`；Android `.apk`/`.aab` |
| 体积 | hello-world ≈ 7 MB 二进制，空闲 ~35 MB 内存（不含 WebKit 子进程） |
| 许可 | MIT |

### 1.1 本机实测

```
$ go run github.com/egoist/mygo/cmd/mygo@latest doctor
✓ go         go version go1.27.4 darwin/arm64
✓ macOS      27.2.1 (WKWebView; macOS 12 or later is required)
✓ xcode      Xcode 27.0
✓ ios sdk    27.0 (simulator 27.0)
✓ simulators 15 available
✓ android    /Users/.../Android/sdk (API 37.0)
✓ adb        Android Debug Bridge version 1.0.41
✓ jdk        25.0.2
! ndk        not found
! bun        not found (optional)
! signing    no Apple Development identity
```

```
$ mygo init -template native demo
$ cd demo && go tool mygo build
  ✓ Compiled darwin/arm64  3.1s
  ✓ Signed demo.app        32ms
  ✓ Creating demo-0.1.0-arm64.dmg
real 7.80s
→ build/darwin-arm64/demo.app (3.2 MB dmg)
```

说明：**工具链可用、构建快、产物小**；移动端只缺 NDK 与签名身份（环境问题，不是框架问题）。

---

## 2. 把 Velarouter 拆成“外壳”和“核”

先明确哪些部分与 UI 框架无关：

| 模块 | 是否与 MyGo 有关 | 应该放哪 |
|---|---|---|
| VRP 协议 / 加密 / 帧格式 | 无关 | Rust（已有）或 Go SDK |
| 信令服务器 | 无关 | Rust 独立二进制（当前实现） |
| 被控端采集 / 编码 / 注入 | 平台相关 | 桌面：Rust（`xcap`/`enigo`）；移动：Swift/Kotlin 原生模块 |
| 主控端连接 / 解码 / 渲染 | **与 UI 强相关** | MyGo 外壳 + Web 前端 |
| 设备列表 / 设置 / 登录 | 与 UI 强相关 | MyGo 外壳 |
| 终端 | 与 UI 相关 | MyGo 的 `terminal` 插件可直接用 |
| 自动更新 | 与 UI 相关 | MyGo 的 `updater` 插件 |

结论：**主控端是 MyGo 的甜点区；被控端需要一个原生/独立进程。**

---

## 3. 三种可选架构

### 方案 A：MyGo 只做壳，媒体走 Go

```
┌──────────────────────── MyGo App ────────────────────────┐
│  WebView (HTML/TS)                                       │
│    ├─ canvas 渲染 JPEG / WebCodecs 解 H.264               │
│    └─ DOM 事件 → 输入消息                                 │
│            ↕ MyGo IPC（Bind / Event / Channel）           │
│  Go: velarouter-go SDK                                   │
│    ├─ WebSocket / UDP                                    │
│    ├─ P-256 + HKDF + AES-GCM（crypto/ecdh + cipher）      │
│    └─ 帧队列 / 统计 / 重连                                 │
└──────────────────────────────────────────────────────────┘
```

- 适合：主控端（Windows / macOS / Linux / iOS / Android 共用一套 TS + Go）。
- 优点：一套 UI 五端复用；Go 的网络与并发足够；WebCrypto 与 Go 都能做加密。
- 缺点：帧要从 Go 复制到 JS（IPC 开销），1080p@30 约 4–6 MB/s，需用 `ArrayBuffer` + 零拷贝通道。

### 方案 B：MyGo + Rust 原生模块（桌面被控端）

```
MyGo App
├─ Go: 窗口 / 托盘 / 菜单 / 设置
├─ rust/ 原生模块（MyGo native module）
│    └─ 复用 crates/vr-agent 的 capture + input + session
└─ 前端：状态面板 / 配对码 / 日志
```

- MyGo 文档明确支持 `rust/` 目录作为 native module，运行于 macOS/Linux/Windows。
- 这意味着 **Velarouter 的 Rust crate 可以直接被 MyGo 应用复用**，不需要重写采集与输入。
- 移动端不能跑 Rust 模块（文档限定桌面），需要 Swift/Kotlin 实现同一组 Go 接口。

### 方案 C：MyGo 做全部（含被控端，纯 Go）

- 桌面：Go 通过 `purego` 调 CGDisplayStream/ScreenCaptureKit、CGEventPost、Win32 GDI/BitBlt、X11。
- 移动：Swift/Kotlin 原生模块。
- 评价：可行但**不建议**——purego 调 ObjC runtime 的维护成本高，且 MyGo 桌面端刻意不用 cgo，
  等于自己造一层平台绑定。Rust 已经有成熟 crate（`xcap`/`enigo`）。

### 推荐

| 角色 | 方案 |
|---|---|
| 主控端（5 端） | **A**（MyGo + Go SDK + Web 前端） |
| 被控端（Win/macOS/Linux） | **B**（MyGo 外壳 + Rust 原生模块，或直接跑独立 `vr-agent` 进程） |
| 被控端（Android） | MyGo + Kotlin 模块（MediaProjection + Accessibility） |
| 被控端（iOS） | 只读共享（ReplayKit 广播扩展），无输入注入 |
| 信令服务器 | 保持 Rust 二进制，不用 MyGo |

---

## 4. Go 客户端 SDK（已实现）

MyGo 应用是 Go，因此需要一份 Go 的 VRP 实现。本仓库已经提供并通过了三级验证：
**已知答案向量 → 与 Rust agent 的真实连接 → 3 帧 JPEG 落盘**。

代码：`sdk/go/velarouter.go`；冒烟工具：`sdk/go/cmd/smoke`。

```
$ go test ./...
ok  github.com/logdns/Velarouter/sdk/go
$ go run ./cmd/smoke -url ws://127.0.0.1:34799/vr -code 654321
handshake ok in 2ms (device=ideadevdeMac-Studio.local)
frame 0: 1280x720 72439 bytes (codec 1)
saved /tmp/go-shot.jpg
ok: 3 frames, 219.8 KiB
```

实现清单（约 480 行，含测试）：

| 组件 | Go 实现 | 代码量 |
|---|---|---|
| WebSocket | `nhooyr.io/websocket` 或 `gorilla/websocket` | ~50 行 |
| P-256 ECDH | `crypto/ecdh`（Go 1.20+） | ~20 行 |
| HKDF-SHA256 | `golang.org/x/crypto/hkdf` | ~10 行 |
| HMAC 证明 | `crypto/hmac` | ~10 行 |
| AES-256-GCM | `crypto/cipher` | ~40 行 |
| JSON 消息 | `encoding/json` | 结构体定义 ~150 行 |
| 媒体帧解析 | `encoding/binary` | ~30 行 |
| **合计** | | **~480 行（已完成）** |

这与 `webviewer/app.js` 是同一套步骤，可以逐行对照实现。
有了它，MyGo 客户端只需：

```go
client, err := velarouter.Dial(ctx, url, code)
for {
    frame, err := client.NextFrame(ctx)   // *Frame{JPEG []byte, Width, Height}
    events.Emit("frame", frame)           // 推给前端
}
```

前端：

```ts
import { events } from "./mygo";

const canvas = document.querySelector("canvas")!;
const ctx = canvas.getContext("2d")!;

events.frame.on(async (f) => {
  const bmp = await createImageBitmap(new Blob([f.jpeg], { type: "image/jpeg" }));
  canvas.width = f.width;
  canvas.height = f.height;
  ctx.drawImage(bmp, 0, 0);
});
```

---

## 5. 原生模块示例（桌面被控端复用 Rust）

Go 侧声明接口：

```go
//go:build darwin || windows || linux

package capture

// Screen 是一个原生模块：由 Rust 实现。
//
//mygo:native
type Screen interface {
    Displays() ([]Display, error)
    Capture(displayID int, maxWidth, maxHeight, quality int) ([]byte, int, int, error)
    Inject(event InputEvent) error
}
```

`rust/src/lib.rs` 实现同一接口（内部直接 `use vr_agent::capture::*`、`vr_agent::input::*`），
MyGo 负责编译 crate、打包进 `.app`/`.exe`，并生成两边都通过的声明。

移动端则用 Swift（ScreenCaptureKit/ReplayKit）与 Kotlin（MediaProjection）实现同一个 Go 接口，
这就是 MyGo 设计里“一个接口，多语言实现”的价值。

---

## 6. 前端渲染：JPEG 还是 WebCodecs

| 方案 | 兼容性 | 延迟 | CPU | 建议 |
|---|---|---|---|---|
| `createImageBitmap(JPEG)` + `drawImage` | 全平台，包括旧 WebView2 | 中 | 解码在浏览器 | **默认** |
| `WebCodecs.VideoDecoder`（H.264） | Chrome/Edge 94+、Safari 16.4+、WKWebView iOS 16.4+ | 低 | 硬解 | 带宽敏感时启用 |
| WASM 解码器 | 全平台 | 高 | 高 | 不推荐 |

MyGo 的 WebView 就是系统 WebView，因此 **WebCodecs 能力 = 系统浏览器能力**。
实现时应先探测：

```ts
const hasWebCodecs = typeof VideoDecoder !== "undefined"
  && (await VideoDecoder.isConfigSupported({ codec: "avc1.42E01E", codedWidth: 1920, codedHeight: 1080 })).supported;
```

不支持就回落到 JPEG 路径（协议本身允许双编码器协商）。

---

## 7. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| 原生模块标注**实验性**，可能破坏性变更 | 中 | 把原生模块隔离在 `native/` 一层，Go 侧只依赖接口；升级时先跑 conformance |
| MyGo 0.10.0，生态年轻 | 中 | UI 用纯 HTML/TS，不深度绑定 MyGo API；必要时 2–3 天迁移到 Tauri/Wails |
| 需要 Go 1.27.3+ | 低 | 团队已有 Go；CI 固定版本 |
| 桌面端无 cgo 限制 | 中 | 被控端采集走 Rust 原生模块（推荐）或独立进程 |
| 移动端背景限制（iOS 挂起 / Android Doze） | 高 | Android 前台服务；iOS 明确“仅前台 + 广播扩展” |
| App Store 对远程桌面类目审核 | 中 | 只做“连接自己的设备”，不内置应用商店/下载执行；提供隐私说明 |
| 帧 IPC 复制开销 | 中 | 二进制 `Channel[[]byte]`，避免 JSON/base64；必要时共享内存 |
| WebCodecs 在部分 WebView2 版本缺失 | 中 | JPEG 回退 + 能力探测 |
| updater 签名/公证 | 中 | MyGo 的 `updater` 插件 + 各平台签名流程（见构建与分发文档） |

---

## 8. 与其他框架对比

| 维度 | MyGo | Tauri 2 | Wails | Flutter | Electron | Qt |
|---|---|---|---|---|---|---|
| 后端语言 | **Go** | Rust | Go | Dart | Node | C++ |
| 桌面不用 cgo | ✅ | — | ❌ | — | — | — |
| 五端统一 | ✅（含 iOS/Android） | 桌面 + 移动 beta | 桌面 | ✅ | 桌面 | 桌面为主 |
| 包体（hello） | **7 MB** | ~5 MB | ~10 MB | ~15 MB | ~120 MB | ~30 MB |
| 原生模块 | Swift/Kotlin/Rust/Zig（实验） | Rust | Go + cgo | 平台通道 | N-API | C++ |
| 终端/更新/DB 插件 | ✅ 内置 | 社区 | 社区 | 社区 | 社区 | 需自建 |
| 成熟度 | 0.10.0 | 2.x 稳定 | 2.x 稳定 | 3.x 稳定 | 稳定 | 稳定 |
| 与本项目协议栈契合 | **高**（Go SDK） | 中（Rust 可复用） | 中 | 低 | 低 | 低 |

对本项目而言：

- 如果团队是 Go 背景、要五端统一、包体小 → **MyGo**。
- 如果团队是 Rust 背景、只要桌面 → **Tauri 2**（可直接复用 `vr-session`，甚至把 agent 作为 sidecar）。
- 如果只要 Windows → 直接 Qt/WinUI 更稳。

---

## 9. 工作量估算（主控端，5 端）

以“能看能控、有设备列表/配对/画质调节/全屏”为 MVP：

| 工作 | 人日 |
|---|---|
| Go SDK（协议 + 加密 + 帧） | 0（已完成） |
| Web 前端（连接、渲染、输入、统计） | 6 |
| MyGo 外壳（窗口、菜单、托盘、设置持久化） | 3 |
| 移动端适配（触摸手势、软键盘、安全区） | 5 |
| 打包/签名/公证/商店材料 | 6 |
| 测试（弱网、后台、权限拒绝、多分辨率） | 5 |
| **合计** | **~25 人日** |

对比自研（各自平台原生 UI + 各自网络栈）：约 70–90 人日。
对比 Electron：约 25 人日，但包体 120 MB、内存 300 MB、移动端缺失。

---

## 10. go / no-go 判定

| 问题 | 答案 | 权重 |
|---|---|---|
| 能做桌面主控端？ | ✅ 实测可构建、可运行 | 必须 |
| 能做移动主控端？ | ✅ 官方支持 iOS/Android 构建 | 必须 |
| 能复用现有 Rust 协议/采集？ | ✅ 通过 Rust native module（桌面） | 高 |
| 能承载 30fps 视频？ | ⚠️ 可以（JPEG/WebCodecs），需注意 IPC 复制 | 高 |
| 能做被控端（全平台）？ | ⚠️ 桌面建议配 Rust 模块；移动需 Kotlin/Swift | 中 |
| 能过应用商店？ | ⚠️ 取决于定位与隐私声明 | 中 |
| 框架是否稳定？ | ⚠️ 0.10.0，原生模块实验性 | 中 |
| 是否锁定？ | 🟢 UI 是标准 Web，可迁移 | 低 |

**判定：有条件采用。**

条件：

1. 只用 MyGo 做**外壳 + 主控端**，不把采集/输入核心写进 MyGo；
2. 原生模块与 UI 之间保持清晰接口，随时可替换；
3. 先做一个 macOS 主控端 PoC（1 周），验证 WebView 解码延迟与 IPC 吞吐；
4. 移动端排期放在桌面之后。

---

## 11. 一周 PoC 验收清单

- [ ] `mygo init` 创建项目，`mygo build` 产出 `.app` 与 `.dmg`
- [ ] Go SDK 完成握手，能解出第一帧 JPEG
- [ ] 1080p@30 连续 10 分钟，CPU < 25%，内存 < 400 MB
- [ ] 输入往返 RTT（本机）< 30 ms
- [ ] 拔网线 10 秒后自动重连
- [ ] 打包体积 < 15 MB
- [ ] 代码中不出现 `import "C"`

七项全过 → 全面采用；任一项失败 → 用 Tauri 2 复用 Rust 核心（迁移成本约 3 人日）。

---

## 12. 参考

- 快速开始：<https://mygo.run/zh/docs/getting-started>
- 架构（purego、无 cgo、IPC）：<https://mygo.run/zh/docs/architecture>
- 原生模块（Swift/Kotlin/Rust/Zig）：<https://mygo.run/zh/docs/native-modules>
- 手机与平板：<https://mygo.run/zh/docs/mobile>
- WebSocket 插件：<https://mygo.run/zh/docs/plugins/websocket>
- 终端插件：<https://mygo.run/zh/docs/plugins/terminal>
- 构建与分发：<https://mygo.run/zh/docs/distribution>
- 自动更新：<https://mygo.run/zh/docs/updates>

> MyGo 的文档与版本会演进；本文的实测数据基于 0.10.0（2026-11）。
