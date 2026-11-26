# iOS / iPadOS 客户端开发文档

> 先明确一件事：**iOS 不能作为被控端接受系统级输入注入。**
> 这是平台限制，不是权限问题。iOS 端的定位是：
> - **主控端**：完整功能（看 + 控别的设备）；
> - **被控端**：只能通过 ReplayKit 广播扩展做“只读共享”，不能远程操作。

---

## 1. 推荐技术栈

| 层 | 选择 |
|---|---|
| 外壳 | SwiftUI（原生）或 MyGo（Go + WKWebView） |
| 渲染 | Canvas/WebCodecs（WebView）或 `AVSampleBufferDisplayLayer`（原生） |
| 网络 | `URLSessionWebSocketTask` / `Network.framework` |
| 加密 | CryptoKit（P-256 + HKDF + AES-GCM） |
| 屏幕共享 | ReplayKit `Broadcast Upload Extension` |
| 分发 | App Store / TestFlight / MDM |

最低系统：iOS 16（WebCodecs 需要 16.4+；低于此版本走 JPEG 路径）。

---

## 2. 主控端：连接与解密

### 2.1 P-256 + HKDF

```swift
import CryptoKit

let priv = P256.KeyAgreement.PrivateKey()
let pub  = priv.publicKey.x963Representation          // 65B: 0x04||X||Y
let peer = try P256.KeyAgreement.PublicKey(x963Representation: agentPub)
let shared = try priv.sharedSecretFromKeyAgreement(with: peer)   // 32B x-coordinate

let key = shared.hkdfDerivedSymmetricKey(
    using: SHA256.self,
    salt: Data("\(sessionId)|\(code)".utf8),
    sharedInfo: Data("velarouter/vrp1/session-key".utf8),
    outputByteCount: 32)

let proof = Data(HMAC<SHA256>.authenticationCode(
    for: Data("viewer|\(sessionId)".utf8), using: key)).base64EncodedString()
```

与 [`../PROTOCOL.md`](../PROTOCOL.md) §3.4 的测试向量对齐后再继续。

### 2.2 AES-GCM

```swift
func iv(direction: UInt8, counter: UInt64) -> Data {
    var d = Data([direction, 0, 0, 0])
    withUnsafeBytes(of: counter.bigEndian) { d.append(contentsOf: $0) }
    return d                                   // 12 bytes
}

let box = try AES.GCM.seal(plaintext,
    nonce: AES.GCM.Nonce(data: iv(direction: 1, counter: txCounter)),
    authenticating: Data("vrp1".utf8))
```

注意：CryptoKit 的 `combined` 是 `nonce||ciphertext||tag`，而协议是 `counter||ciphertext||tag`，
所以**不要**直接用 `box.combined`，要自己拼：

```swift
var wire = Data()
withUnsafeBytes(of: txCounter.bigEndian) { wire.append(contentsOf: $0) }
wire.append(box.ciphertext)
wire.append(box.tag)
```

### 2.3 WebSocket

```swift
let task = URLSession.shared.webSocketTask(with: URL(string: "ws://192.168.1.10:34789/vr")!)
task.resume()
try await task.send(.string(helloJson))

while true {
    let msg = try await task.receive()
    switch msg {
    case .data(let data): try await handleBinary(data)
    case .string(let text): try await handleText(text)
    @unknown default: break
    }
}
```

---

## 3. 主控端：渲染

### 3.1 方案 A：WKWebView + Canvas（推荐，复用 Web 前端）

`webviewer/app.js` 可以直接跑在 WKWebView 里（WebCrypto 完整支持）。
优点：一套前端五端复用；缺点：JS 桥接有 ~1 帧延迟。

如果要从原生推送帧给 WebView：

```swift
webView.evaluateJavaScript("window.__vrFrame('\(base64)')")
```

base64 会放大 33%，更好的做法是 `WKURLSchemeHandler` 或 `postMessage` + `ArrayBuffer`。

### 3.2 方案 B：原生 AVSampleBufferDisplayLayer（H.264）

```swift
let layer = AVSampleBufferDisplayLayer()
layer.videoGravity = .resizeAspect

// Annex-B → CMSampleBuffer
var format: CMFormatDescription?
CMVideoFormatDescriptionCreateFromH264ParameterSets(
    allocator: nil,
    parameterSetCount: 2,
    parameterSetPointers: [sps, pps],
    parameterSetSizes: [sps.count, pps.count],
    nalUnitHeaderLength: 4,
    formatDescriptionOut: &format)

let sample = CMSampleBufferCreateReadyWithImageBuffer(...)
layer.enqueue(sample)
```

- 必须显式设置 `layer.flush()` 在 IDR 前清空；
- 低延迟：`kCMSampleAttachmentKey_DisplayImmediately = true`；
- 音频用 `AVAudioEngine` + `AVAudioPlayerNode`（协议目前无音频）。

### 3.3 缩放与安全区

- iPad 分屏时窗口尺寸变化频繁，用 `GeometryReader` + `aspectRatio(.fit)`；
- 刘海/灵动岛：`safeAreaInsets` 不要把画面裁掉；
- 全屏：`prefersHomeIndicatorAutoHidden`、`prefersStatusBarHidden`。

---

## 4. 主控端：输入

### 4.1 触摸

iOS 没有“绝对坐标鼠标”，需要把触摸手势翻译成协议事件：

| 手势 | 映射 |
|---|---|
| 单指拖动 | `mouse_move`（相对或绝对） |
| 单指轻点 | `mouse_button{left,down/up}` |
| 双指轻点 | `mouse_button{right}` |
| 双指拖动 | `wheel` |
| 长按 + 拖动 | 左键拖拽 |
| 三指轻点 | 中键 |

```swift
override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
    guard let t = touches.first else { return }
    let p = t.location(in: canvas)
    let nx = UInt16(max(0, min(65535, p.x / canvas.bounds.width * 65535)))
    let ny = UInt16(max(0, min(65535, p.y / canvas.bounds.height * 65535)))
    send(InputEvent.mouseMove(x: nx, y: ny, display: 0))
}
```

### 4.2 键盘

- 软键盘：用 `UITextInput` 或隐藏 `UITextField` 捕获 `insertText`，发 `InputEvent::Text`；
- 硬件键盘（iPad）：`UIKeyCommand` / `pressesBegan`，把 `UIKeyboardHIDUsage` 映射成 W3C code；
- 快捷键：`UIKeyCommand(input: "c", modifierFlags: .command, action: #selector(copyRemote))`。

### 4.3 手柄

`GameController` 框架，把 `GCController` 的按键映射到 `InputEvent::Key`（协议无手柄专用消息，
用键盘码兜底，或扩展 `InputEvent`）。

---

## 5. 被控端：ReplayKit 只读共享

### 5.1 架构

```
Velarouter.app
  └─ RPSystemBroadcastPickerView / RPScreenRecorder
       └─ Broadcast Upload Extension（独立进程）
            └─ 接收 CMSampleBuffer → 编码 → VRP → 主控端
```

- 广播扩展有 **50 MB 内存上限**，不能做 4K 软编；
- 只能拿到屏幕帧和麦克风（`RPSampleBufferType`），**不能注入输入**；
- 扩展生命周期由系统管理，主 App 可以在后台甚至被挂起。

### 5.2 关键代码

```swift
class SampleHandler: RPBroadcastSampleHandler {
    override func broadcastStarted(withSetupInfo info: [String: NSObject]?) {
        // 建立 VRP 会话（只发送，不接收输入）
    }
    override func processSampleBuffer(_ sb: CMSampleBuffer, with type: RPSampleBufferType) {
        guard type == .video else { return }
        // CVPixelBuffer → VideoToolbox → VRP media frame
    }
    override func broadcastFinished() {
        // 关闭连接
    }
}
```

### 5.3 限制清单

| 限制 | 说明 |
|---|---|
| 不能注入输入 | 系统无 API |
| 不能采集其他 App 的音频（除麦克风） | ReplayKit 限制 |
| 扩展 50 MB 内存 | 只能硬编 + 低分辨率 |
| 用户必须手动开始广播 | 无法静默启动 |
| 屏幕锁定后停止 | 系统行为 |
| App Store 审核 | 需说明用途 |

---

## 6. 网络与本地网络权限（很重要）

iOS 14+ 访问局域网设备需要：

1. `Info.plist`：

```xml
<key>NSLocalNetworkUsageDescription</key>
<string>Velarouter 需要连接同一局域网内的设备以建立直连。</string>
<key>NSBonjourServices</key>
<array>
  <string>_velarouter._tcp</string>
</array>
```

2. 首次访问会弹“允许查找本地网络设备”，用户拒绝后只能走中继；
3. ATS：`ws://` 默认被禁止，需在 `Info.plist` 配置例外（仅调试）或全用 `wss://`：

```xml
<key>NSAppTransportSecurity</key>
<dict>
  <key>NSAllowsLocalNetworking</key><true/>
</dict>
```

---

## 7. 打包与上架

### 7.1 签名

```bash
xcodebuild -project Velarouter.xcodeproj -scheme Velarouter \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath build/Velarouter.xcarchive archive
xcodebuild -exportArchive -archivePath build/Velarouter.xcarchive \
  -exportOptionsPlist ExportOptions.plist -exportPath build/
```

### 7.2 App Store 注意事项

| 条款 | 影响 |
|---|---|
| 2.5.2 不得下载/执行代码 | 远程桌面可以，但不能内置应用商店/脚本执行 |
| 4.7 小程序/插件 | 如果内嵌 Web 应用需遵守 |
| 5.1.1 数据收集 | 必须有隐私清单（`PrivacyInfo.xcprivacy`） |
| 5.1.2 数据使用 | 屏幕内容属于敏感数据，需明确同意 |
| 加密出口 | 使用标准加密需在 App Store Connect 回答出口合规问题（通常豁免） |
| 辅助功能 | iOS 端不使用 Accessibility 注入，避免审核风险 |

### 7.3 隐私清单

```xml
<!-- PrivacyInfo.xcprivacy -->
<dict>
  <key>NSPrivacyCollectedDataTypes</key><array/>
  <key>NSPrivacyAccessedAPITypes</key>
  <array>
    <dict>
      <key>NSPrivacyAccessedAPIType</key>
      <string>NSPrivacyAccessedAPICategoryUserDefaults</string>
      <key>NSPrivacyAccessedAPITypeReasons</key>
      <array><string>CA92.1</string></array>
    </dict>
  </array>
  <key>NSPrivacyTracking</key><false/>
</dict>
```

---

## 8. 测试清单

- [ ] 通过 §3.4 测试向量（CryptoKit 实现一致性）
- [ ] 局域网权限弹窗与拒绝路径
- [ ] `ws://` 与 `wss://` 均可用
- [ ] iPad 分屏 / 旋转 / 台前调度
- [ ] 硬件键盘（妙控键盘）所有修饰键
- [ ] 中文输入法提交整段文本
- [ ] 触摸手势（点击/拖动/双指/长按）
- [ ] 后台 30 秒后回来能恢复
- [ ] 低电量模式下的帧率降级
- [ ] ReplayKit 广播扩展 30 分钟不崩（内存）
- [ ] App Store 隐私清单通过审核

---

## 9. 已知坑

| 坑 | 现象 | 解决 |
|---|---|---|
| `combined` 直接用 | 对端解密失败 | 自己拼 `counter||ct||tag` |
| `ws://` 被 ATS 拦 | 连接失败 | `NSAllowsLocalNetworking` |
| 本地网络权限 | 局域网连不上 | 首次必须允许，拒绝后只能中继 |
| WKWebView 后台 | 定时器被挂起 | 用原生网络层，别依赖 JS 定时器 |
| 视频层黑屏 | 未 flush / 缺 SPS-PPS | IDR 前 `layer.flush()` |
| 广播扩展 OOM | 被系统杀 | 限制 720p + 硬编 |
| 触摸坐标 | 偏移 | 用 canvas 的 bounds，不用 view 的 frame |
| 屏幕旋转 | 画面拉伸 | 用 `videoGravity = .resizeAspect` |
