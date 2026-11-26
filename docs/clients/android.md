# Android 客户端开发文档

> 目标：实现符合 VRP/1 的 Android 主控端与被控端。
> Android 是唯一“被控端能力完整但需要额外授权”的移动平台：
> MediaProjection 采集 + 无障碍注入 + 前台服务。

---

## 1. 推荐技术栈

| 层 | 选择 |
|---|---|
| 外壳 | Kotlin + Compose（或 MyGo + WebView） |
| 渲染 | `SurfaceView` + `MediaCodec`（H.264）或 Canvas（JPEG） |
| 采集 | `MediaProjection` + `VirtualDisplay` + `ImageReader` |
| 编码 | `MediaCodec`（H.264/HEVC，硬编） |
| 注入 | `AccessibilityService.dispatchGesture` + 自定义输入法 |
| 加密 | JCE：`KeyPairGenerator("EC")` + `KeyAgreement` + `HmacSHA256` + `AES/GCM/NoPadding` |
| 网络 | OkHttp WebSocket / `java.net.http`（API 28+） |
| 分发 | Google Play（AAB）/ 企业 MDM |

最低 API：26（`dispatchGesture` 需要 24，前台服务类型需要 29+，建议 26 起步）。

---

## 2. 主控端

### 2.1 握手（JCE）

```kotlin
val kpg = KeyPairGenerator.getInstance("EC")
kpg.initialize(ECGenParameterSpec("secp256r1"))
val kp = kpg.generateKeyPair()

// 未压缩 SEC1 = 0x04 || X || Y
val pub = (kp.public as ECPublicKey).w.let { w ->
    byteArrayOf(0x04) + toFixed(w.affineX, 32) + toFixed(w.affineY, 32)
}

val ka = KeyAgreement.getInstance("ECDH")
ka.init(kp.private)
ka.doPhase(agentPublicKey, true)
val shared = ka.generateSecret()            // 32 bytes
```

HKDF 需要自己实现（JCE 没有）：

```kotlin
fun hkdf(ikm: ByteArray, salt: ByteArray, info: ByteArray, len: Int = 32): ByteArray {
    val mac = Mac.getInstance("HmacSHA256")
    mac.init(SecretKeySpec(salt, "HmacSHA256"))
    val prk = mac.doFinal(ikm)
    mac.init(SecretKeySpec(prk, "HmacSHA256"))
    mac.update(info); mac.update(0x01)
    return mac.doFinal().copyOf(len)
}
```

AES-GCM：

```kotlin
val cipher = Cipher.getInstance("AES/GCM/NoPadding")
cipher.init(Cipher.DECRYPT_MODE, SecretKeySpec(key, "AES"),
            GCMParameterSpec(128, iv))       // iv = dir||000000||counter
cipher.updateAAD("vrp1".toByteArray())
val plain = cipher.doFinal(wire, 8, wire.size - 8)
```

### 2.2 渲染

- JPEG：`BitmapFactory.decodeByteArray` → `Canvas.drawBitmap`；
  在 `SurfaceView` 的 `SurfaceHolder` 上绘制，避免 `ImageView` 的额外拷贝。
- H.264：`MediaCodec.createDecoderByType("video/avc")` + `configure(format, surface, ...)`，
  `releaseOutputBuffer(index, true)` 直接上屏。
- `MediaCodec` 必须处理 `INFO_OUTPUT_FORMAT_CHANGED` 与关键帧。

### 2.3 输入

- 触摸：`MotionEvent` → 归一化 → `InputEvent::Touch`（多点）；
- 软键盘：`InputConnection` / `commitText` → `InputEvent::Text`；
- 物理键盘：`KeyEvent` 的 `scanCode` 映射到 W3C `code`；
- 手柄：`onGenericMotionEvent` + `InputDevice`。

### 2.4 前台服务（主控端也需要，防止后台断连）

```xml
<service android:name=".RemoteSessionService"
    android:foregroundServiceType="dataSync" android:exported="false"/>
```

```kotlin
startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
```

Android 14 起 `dataSync` 有每日时长限制；远程会话应改用
`FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE` 或 `mediaProjection`（被控端）。

---

## 3. 被控端

### 3.1 MediaProjection 采集

```kotlin
val mgr = getSystemService(MediaProjectionManager::class.java)
startActivityForResult(mgr.createScreenCaptureIntent(), REQ_CAPTURE)

// onActivityResult
val projection = mgr.getMediaProjection(resultCode, data)
val display = projection.createVirtualDisplay(
    "vr", width, height, densityDpi,
    DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
    surface, null, handler)
```

配合 `ImageReader` 取像素：

```kotlin
val reader = ImageReader.newInstance(width, height, PixelFormat.RGBA_8888, 2)
virtualDisplay.surface = reader.surface
reader.setOnImageAvailableListener({ r ->
    val image = r.acquireLatestImage()
    val plane = image.planes[0]
    val rowStride = plane.rowStride          // 可能 > width*4
    val pixelStride = plane.pixelStride
    // 拷贝到 bitmap 时按 rowStride 处理，否则图像斜切
    image.close()
}, handler)
```

要点：

- Android 14 起每次 `createScreenCaptureIntent` 只授权一次，不能复用 token；
- `rowStride` 通常有 64 字节对齐，必须逐行拷贝；
- 采集尺寸建议 = 屏幕物理尺寸的一半或 1080p，避免 4K 软编卡死；
- 屏幕旋转时 `VirtualDisplay` 需要 resize（`virtualDisplay.resize()` + `setSurface()`）。

### 3.2 MediaCodec 硬编

```kotlin
val format = MediaFormat.createVideoFormat("video/avc", w, h).apply {
    setInteger(MediaFormat.KEY_COLOR_FORMAT,
        MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
    setInteger(MediaFormat.KEY_BIT_RATE, 6_000_000)
    setInteger(MediaFormat.KEY_FRAME_RATE, 30)
    setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
    setInteger(MediaFormat.KEY_BITRATE_MODE,
        MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR)
    setInteger(MediaFormat.KEY_PROFILE,
        MediaCodecInfo.CodecProfileLevel.AVCProfileHigh)
}
val codec = MediaCodec.createEncoderByType("video/avc")
codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
val inputSurface = codec.createInputSurface()   // 交给 VirtualDisplay
codec.start()
```

- 用 `Surface` 输入（零拷贝），不要让 `VirtualDisplay` 输出到 `ImageReader` 再编码；
- 输出是 AVCC（长度前缀），协议要 Annex-B，需要转换；
- 部分机型 `KEY_BITRATE_MODE` 不支持 CBR，需要回退 VBR；
- 编码器崩溃（`MediaCodec.CodecException`）要能重建。

### 3.3 输入注入：Android 的现实

Android **没有**普通 App 可用的全局输入注入 API。

| 方式 | 能力 | 条件 |
|---|---|---|
| `AccessibilityService.dispatchGesture` | 点击/滑动/多指 | 用户手动开启无障碍 |
| 自定义输入法（IME） | 文本/按键（当前焦点应用） | 用户切换输入法 |
| `InputManager.injectInputEvent` | 全局 | 系统签名 / root |
| `adb shell input` | 全局 | 仅调试 |
| MDM / Device Owner | 部分 | 企业设备 |

**推荐组合**：

1. 触摸：`dispatchGesture`（覆盖 90% 场景）
2. 文本：内置一个极简 IME（`Velarouter Keyboard`），通过 `InputConnection.commitText` 发送
3. 系统键：`performGlobalAction(GLOBAL_ACTION_BACK/HOME/RECENTS)`
4. 游戏手柄：无法注入，只能做“屏幕上的虚拟按键”（画在采集层之下，用触摸注入）

```kotlin
class RemoteAccessibilityService : AccessibilityService() {
    fun tap(x: Float, y: Float) {
        val path = Path().apply { moveTo(x, y) }
        dispatchGesture(GestureDescription.Builder()
            .addStroke(GestureDescription.StrokeDescription(path, 0, 50))
            .build(), null, null)
    }
}
```

`dispatchGesture` 的坐标是**屏幕绝对坐标**，需要把协议归一化坐标反算成物理像素。

### 3.4 前台服务

```xml
<uses-permission android:name="android.permission.FOREGROUND_SERVICE"/>
<uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PROJECTION"/>
<uses-permission android:name="android.permission.POST_NOTIFICATIONS"/>

<service android:name=".ControlledService"
    android:foregroundServiceType="mediaProjection"
    android:exported="false"/>
```

```kotlin
val notification = Notification.Builder(this, CHANNEL)
    .setContentTitle("Velarouter 正在共享屏幕")
    .setSmallIcon(R.drawable.ic_vr)
    .setOngoing(true)
    .build()
startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
```

Android 13+ 需要 `POST_NOTIFICATIONS` 运行时权限，否则前台服务通知不显示（服务仍可运行）。

### 3.5 保活与 Doze

| 问题 | 处理 |
|---|---|
| Doze 断网 | 前台服务 + `WAKE_LOCK`（谨慎） |
| 厂商杀后台（MIUI/EMUI/ColorOS） | 引导用户加白名单；提供“自启动”设置入口 |
| 电池优化 | `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`（需审核） |
| 网络切换 | 监听 `ConnectivityManager`，重连 |

---

## 4. 权限与 Play 政策

### 4.1 权限清单

```xml
<uses-permission android:name="android.permission.INTERNET"/>
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE"/>
<uses-permission android:name="android.permission.ACCESS_WIFI_STATE"/>
<uses-permission android:name="android.permission.FOREGROUND_SERVICE"/>
<uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PROJECTION"/>
<uses-permission android:name="android.permission.POST_NOTIFICATIONS"/>
<uses-permission android:name="android.permission.RECORD_AUDIO"/>   <!-- 可选麦克风 -->

<service android:name=".RemoteAccessibilityService"
    android:permission="android.permission.BIND_ACCESSIBILITY_SERVICE"
    android:exported="false">
    <intent-filter>
        <action android:name="android.accessibilityservice.AccessibilityService"/>
    </intent-filter>
    <meta-data android:name="android.accessibilityservice"
        android:resource="@xml/accessibility_config"/>
</service>
```

`accessibility_config.xml`：

```xml
<accessibility-service
    android:accessibilityEventTypes="typeAllMask"
    android:accessibilityFeedbackType="feedbackGeneric"
    android:canPerformGestures="true"
    android:notificationTimeout="100"
    android:description="@string/accessibility_description"/>
```

### 4.2 Google Play 审核要点

| 政策 | 要求 |
|---|---|
| Accessibility API | 必须在 Play Console 声明用途；仅用于用户明确授权的远程控制 |
| MediaProjection | 必须使用前台服务 + 明确通知 |
| 敏感权限 | 隐私政策必须说明屏幕内容如何处理（不上传服务器） |
| 数据安全表单 | 声明“屏幕内容端到端加密、不收集” |
| 远程桌面类目 | 允许，但不得用于监控他人/规避系统限制 |

审核被拒的高频原因：无障碍用途描述含糊、没有显著的“正在共享”指示、
隐私政策缺失。建议在应用内做一个常驻的“共享中”悬浮提示。

---

## 5. 客户端接入（Kotlin）

```kotlin
val ws = OkHttpClient().newWebSocket(
    Request.Builder().url("ws://192.168.1.10:34789/vr?sid=$sid").build(),
    object : WebSocketListener() {
        override fun onMessage(ws: WebSocket, text: String) { /* 握手 JSON */ }
        override fun onMessage(ws: WebSocket, bytes: ByteString) { /* 密文 */ }
    })

ws.send("""{"t":"hello","version":1,"role":"viewer","session_id":"$sid","app":"velarouter-android/0.1"}""")
```

坐标：

```kotlin
fun norm(v: Float, extent: Int): Int =
    ((v / extent).coerceIn(0f, 1f) * 65535f).roundToInt()
```

---

## 6. 测试清单

- [ ] 通过 §3.4 测试向量（JCE 实现一致性）
- [ ] Android 12/13/14 的 MediaProjection 授权差异
- [ ] 屏幕旋转、折叠屏展开/合拢
- [ ] 多显示器（DeX / 外接屏）
- [ ] 无障碍注入在不同 Launcher / 输入法下可用
- [ ] 自定义输入法切换与文本提交
- [ ] 前台服务通知与 Android 13 通知权限
- [ ] 厂商杀后台（小米/华为/OPPO/vivo）白名单引导
- [ ] 弱网（3G/丢包 5%）下的帧率与重连
- [ ] 蓝牙耳机麦克风（可选音频）
- [ ] Play 审核的隐私表单与无障碍声明

---

## 7. 已知坑

| 坑 | 现象 | 解决 |
|---|---|---|
| `rowStride` 未处理 | 画面斜切/花屏 | 按 `rowStride` 逐行拷贝 |
| MediaProjection token 复用 | Android 14 抛异常 | 每次重新申请 |
| `createInputSurface` 与 `ImageReader` 同时用 | 编码器冲突 | 只选一条链路 |
| `dispatchGesture` 坐标 | 点击偏移 | 用 `DisplayMetrics` 的真实像素 |
| 无障碍被系统关闭 | 注入静默失败 | 每次会话检查 `AccessibilityManager` |
| 输入法抢占 | 文本发到错误应用 | 只向当前焦点 `InputConnection` 提交 |
| 厂商省电 | 后台 5 分钟断连 | 前台服务 + 白名单 + 心跳 |
| HEVC 编码器缺失 | `configure` 抛异常 | 探测 `MediaCodecList`，回退 H.264/JPEG |
| Play 审核 | 无障碍用途被质疑 | 提供录制演示 + 明确文案 |
