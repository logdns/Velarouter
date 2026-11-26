# macOS 客户端开发文档

> 目标：实现符合 VRP/1 的 macOS 主控端与被控端。
> 协议：[`../PROTOCOL.md`](../PROTOCOL.md)；权限模型：[`../SECURITY.md`](../SECURITY.md)。

---

## 1. 推荐技术栈

| 层 | 选择 | 理由 |
|---|---|---|
| 外壳 | MyGo（Go + WKWebView）或 SwiftUI / AppKit | 系统 WebView，包体小 |
| 渲染 | Canvas 2D（JPEG）/ `AVSampleBufferDisplayLayer`（H.264） | 硬解、能效好 |
| 采集 | **ScreenCaptureKit**（macOS 12.3+），回退 `CGDisplayStream` | 系统唯一“受支持”的高帧率路径 |
| 编码 | VideoToolbox（`VTCompressionSession`） | 硬编，H.264/HEVC |
| 输入 | `CGEventPost` / `CGEventCreateMouseEvent` | 无需驱动 |
| 权限 | TCC：屏幕录制 + 辅助功能 | 必须处理拒绝路径 |
| 分发 | Developer ID + 公证（notarization） | Gatekeeper |
| 更新 | Sparkle 2 或 MyGo updater | 签名校验 |

本仓库的 `vr-agent` 可以直接在 macOS 上运行（`xcap` 走 ScreenCaptureKit，`enigo` 走 CGEvent）。
只有在需要 HEVC / 4:4:4 / HDR 时才需要写 Swift。

---

## 2. 被控端：采集

### 2.1 ScreenCaptureKit（推荐）

```swift
import ScreenCaptureKit

let content = try await SCShareableContent.excludingDesktopWindows(false,
                                                                  onScreenWindowsOnly: true)
guard let display = content.displays.first else { return }

let filter = SCContentFilter(display: display, excludingWindows: [])
let config = SCStreamConfiguration()
config.width  = display.width  * 2      // Retina：像素尺寸
config.height = display.height * 2
config.minimumFrameInterval = CMTime(value: 1, timescale: 30)
config.pixelFormat = kCVPixelFormatType_32BGRA
config.showsCursor = true
config.queueDepth = 5                   // 关键：太小会丢帧，太大会增加延迟

let stream = SCStream(filter: filter, configuration: config, delegate: self)
try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
try await stream.startCapture()
```

要点：

- **队列深度**（`queueDepth`）决定延迟：1–3 适合远程桌面，5 以上会累积；
- Retina 下 `display.width` 是逻辑像素，需要 ×2 得到物理像素；
- `SCContentFilter` 支持“排除自己窗口”，避免递归镜像；
- 首次调用会触发 TCC 弹窗；用户拒绝后 `SCShareableContent` 抛错，必须引导到系统设置；
- macOS 15 起需要在 `Info.plist` 声明 `NSScreenCaptureDescription`（用户可见原因）。

### 2.2 CGDisplayStream（兼容旧系统）

```swift
let props: [String: Any] = [
  kCGDisplayStreamSourceRect: CGRect(x: 0, y: 0, width: w, height: h),
  kCGDisplayStreamMinimumFrameTime: 1.0 / 30.0,
  kCGDisplayStreamShowCursor: true,
  kCGDisplayStreamPreserveAspectRatio: true,
]
let stream = CGDisplayStream(dispatchQueueDisplay: displayID, outputWidth: w,
                             outputHeight: h, pixelFormat: kCVPixelFormatType_32BGRA,
                             properties: props) { status, _, image, _ in ... }
```

- macOS 14 起被标记 deprecated，但仍可用；
- 不支持 HDR，不建议新项目使用。

### 2.3 窗口采集

`SCContentFilter(desktopIndependentWindow:)` 可以只采一个窗口，
适合“只共享某个应用”的场景（对应协议里的 `CT_WINDOW` 通道）。

### 2.4 屏幕权限状态检查

```swift
if !CGPreflightScreenCaptureAccess() {
    CGRequestScreenCaptureAccess()   // 只会弹一次
}
```

拒绝后**无法**通过代码重新申请，只能引导：
`x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture`。

---

## 3. 被控端：编码

### 3.1 VideoToolbox H.264

```swift
var session: VTCompressionSession?
VTCompressionSessionCreate(allocator: nil,
    width: Int32(w), height: Int32(h),
    codecType: kCMVideoCodecType_H264,
    encoderSpecification: [kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: true] as CFDictionary,
    imageBufferAttributes: nil,
    compressedDataAllocator: nil,
    outputCallback: nil, refcon: nil,
    compressionSessionOut: &session)

VTSessionSetProperty(session!, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
VTSessionSetProperty(session!, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse) // 关闭 B 帧
VTSessionSetProperty(session!, key: kVTCompressionPropertyKey_ProfileLevel,
                     value: kVTProfileLevel_H264_High_AutoLevel)
VTSessionSetProperty(session!, key: kVTCompressionPropertyKey_AverageBitRate,
                     value: bitrate as CFNumber)
VTSessionSetProperty(session!, key: kVTCompressionPropertyKey_MaxKeyFrameInterval,
                     value: 60 as CFNumber)
```

- `RealTime = true` 是远程桌面的关键：让编码器优先低延迟而不是压缩率；
- 输出是 AVCC（长度前缀），协议要求 **Annex-B**，需要转换（把 4 字节长度换成 `00 00 00 01`）；
- `VTCompressionSessionEncodeFrame` 的 `frameProperties` 可强制 IDR：

```swift
VTCompressionSessionEncodeFrame(session!, imageBuffer: pb, presentationTimeStamp: pts,
    duration: .invalid, frameProperties: [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary,
    infoFlagsOut: nil) { status, flags, sampleBuffer in ... }
```

### 3.2 HEVC / HDR

- `kCMVideoCodecType_HEVC`，需要 macOS 11+ 与硬件支持；
- HDR 需要 `kVTCompressionPropertyKey_HDRMetadataInsertionMode` + 10-bit `x420`;
- 协议 `codec` 字段用 `h265`，`Capabilities.codecs` 声明后由主控端选择。

### 3.3 回退 JPEG

与 Rust 实现一致：`CGImage` → `CGImageDestination` (JPEG) 或 `NSBitmapImageRep`。
质量 0.6，1600×900 约 100–150 KB。

---

## 4. 被控端：输入注入

### 4.1 鼠标

```swift
let move = CGEvent(mouseEventSource: nil, mouseType: .mouseMoved,
                   mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left)
move?.post(tap: .cghidEventTap)

let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown,
                   mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left)
down?.post(tap: .cghidEventTap)
```

- 坐标是 **全局显示器坐标**（左上角为原点，主屏可为负）；
- 滚轮：`CGEvent(scrollWheelEvent2Source:units:wheelCount:wheel1:wheel2:wheel3:)`；
- 高精度滚动用 `kCGScrollWheelEventIsContinuous = 1`。

### 4.2 键盘

```swift
let src = CGEventSource(stateID: .hidSystemState)
let down = CGEvent(keyboardEventSource: src, virtualKey: vk, keyDown: true)
down?.flags = [.maskShift]          // 修饰键
down?.post(tap: .cghidEventTap)
```

W3C `code` → macOS 虚拟键码（`kVK_ANSI_A = 0x00` …）需要一张 100+ 项的表；
注意：

- macOS 的虚拟键码是 **物理布局**，与字符无关（`kVK_ANSI_A` 始终是 A 键位置）；
- 修饰键用 flags，而不是单独发 `kVK_Shift`；
- 中文输入法：合成字符用 `CGEventKeyboardSetUnicodeString`：

```swift
let e = CGEvent(keyboardEventSource: src, virtualKey: 0, keyDown: true)!
var chars = Array("你".utf16)
e.keyboardSetUnicodeString(stringLength: chars.count, unicodeString: &chars)
e.post(tap: .cghidEventTap)
```

### 4.3 权限

- 辅助功能（Accessibility）：`AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt: true])`；
- 未授权时 `CGEvent.post` 静默失败（不报错），必须**主动检测并提示**；
- 屏幕录制与辅助功能是**两个独立**的 TCC 权限。

### 4.4 Secure Input

当有应用开启 Secure Input（密码框、终端 `sudo`）时，系统会阻止合成键盘事件，
`CGEventSourceKeyState` 可检测：`IsSecureEventInputEnabled()`。
此时应提示主控端“目标正在安全输入，键盘不可用”。

---

## 5. 主控端

### 5.1 渲染

- JPEG：`NSImage`/`CGImage` → `CALayer.contents`；
- H.264：`AVSampleBufferDisplayLayer`，把 Annex-B 转成 `CMSampleBuffer`（需 SPS/PPS）；
- 60fps 时用 `CVDisplayLink` 或 `CADisplayLink` 驱动，避免 `drawRect` 抖动。

### 5.2 输入捕获

- 普通 `NSView.keyDown` 无法拿到系统快捷键（`Cmd+Tab`）；
- 需要 `CGEventTap`（`kCGHIDEventTap`）做全局捕获，但需要辅助功能权限；
- 更简单的做法：进入“远程控制模式”时使用 `NSEvent.addGlobalMonitorForEvents`，
  并在 UI 上提供“发送 Cmd+Tab”等按钮。

### 5.3 全屏与多屏

- `NSWindow.toggleFullScreen`；
- 主控端窗口跨屏时，把鼠标位置归一化到**被控端**的画面尺寸，而不是本地窗口；
- Retina 下 canvas 的 `devicePixelRatio` 要乘进去，否则画面糊。

---

## 6. 打包、签名与公证

### 6.1 签名

```bash
codesign --deep --force --options runtime \
  --sign "Developer ID Application: Example (TEAMID)" \
  --entitlements Velarouter.entitlements \
  Velarouter.app
```

`entitlements` 至少包含：

```xml
<key>com.apple.security.device.camera</key><false/>
<key>com.apple.security.network.client</key><true/>
<key>com.apple.security.network.server</key><true/>   <!-- 被控端监听 -->
<key>com.apple.security.cs.allow-jit</key><true/>     <!-- WebView2/JS -->
```

### 6.2 公证

```bash
ditto -c -k --keepParent Velarouter.app Velarouter.zip
xcrun notarytool submit Velarouter.zip --keychain-profile "AC_PASSWORD" --wait
xcrun stapler staple Velarouter.app
```

未公证的 app 在新系统上会被 Gatekeeper 直接拒绝。

### 6.3 权限描述

`Info.plist` 需要（否则 TCC 弹窗文案为空，审核会拒）：

```xml
<key>NSScreenCaptureDescription</key>
<string>Velarouter 需要录制屏幕以共享给你的其他设备。</string>
<key>NSAppleEventsUsageDescription</key>
<string>Velarouter 需要发送输入事件以远程操作这台 Mac。</string>
```

### 6.4 开机自启

用 `SMAppService.mainApp.register()`（macOS 13+）或 LaunchAgent。
被控端建议 LaunchAgent（用户级），不要用 LaunchDaemon（拿不到桌面会话）。

---

## 7. 客户端接入（Swift）

```swift
import CryptoKit

// 1. ECDH P-256
let priv = P256.KeyAgreement.PrivateKey()
let pubData = priv.publicKey.x963Representation          // 65 bytes, 0x04||X||Y
let peer = try P256.KeyAgreement.PublicKey(x963Representation: agentPub)
let shared = try priv.sharedSecretFromKeyAgreement(with: peer)

// 2. HKDF
let salt = Data("\(sessionId)|\(code)".utf8)
let key = shared.hkdfDerivedSymmetricKey(using: SHA256.self,
                                         salt: salt,
                                         sharedInfo: Data("velarouter/vrp1/session-key".utf8),
                                         outputByteCount: 32)

// 3. 证明
let authCode = HMAC<SHA256>.authenticationCode(for: Data("viewer|\(sessionId)".utf8), using: key)
let proof = Data(authCode).base64EncodedString()

// 4. AES-GCM
let sealed = try AES.GCM.seal(plaintext,
    nonce: AES.GCM.Nonce(data: iv(direction: 1, counter: counter)),
    authenticating: Data("vrp1".utf8))
```

注意：`CryptoKit` 的 `sharedSecretFromKeyAgreement` 返回的是 **x 坐标**（32 字节），与协议一致。

---

## 8. 测试清单

- [ ] 首次运行 TCC 弹窗（屏幕录制 / 辅助功能）文案正确
- [ ] 拒绝权限后 UI 有明确引导，不黑屏卡死
- [ ] Retina（2x）与普通屏画面清晰，坐标不偏移
- [ ] 多显示器（含负坐标、旋转 90°）正确
- [ ] 安全输入状态下键盘事件被拦截并有提示
- [ ] 全屏应用（游戏/视频）采集正常
- [ ] 休眠唤醒后采集恢复
- [ ] 公证后在其他 Mac 上可直接打开
- [ ] Universal Binary（arm64 + x86_64）
- [ ] 被控端 LaunchAgent 在用户登录后启动

---

## 9. 已知坑

| 坑 | 现象 | 解决 |
|---|---|---|
| `SCShareableContent` 返回空 | 未授权或屏幕锁定 | 检查 TCC，锁屏时无法采集 |
| `queueDepth` 太大 | 画面延迟 1–2 秒 | 设为 1–3 |
| CGEvent 静默失败 | 点击无反应 | `AXIsProcessTrusted` 检查 |
| 虚拟键码 | 字符错乱 | 用物理键码表 + Unicode 文本事件 |
| VideoToolbox 输出 AVCC | 对端无法解码 | 转 Annex-B（4 字节长度 → start code） |
| `codesign --deep` | 公证失败 | 逐层签名（frameworks → helpers → app） |
| LaunchDaemon | 采集黑屏 | 改用 LaunchAgent（用户会话） |
| 屏幕录制权限“幽灵” | 重装后仍显示已授权 | `tccutil reset ScreenCapture <bundle-id>` |
| WebView 内存 | 长时间会话增长 | 定期重建 WebView 或限制帧缓存 |
