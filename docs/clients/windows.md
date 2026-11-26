# Windows 客户端开发文档

> 目标：实现一个符合 VRP/1 的 Windows 主控端与被控端。
> 协议：[`../PROTOCOL.md`](../PROTOCOL.md)；权限与威胁模型：[`../SECURITY.md`](../SECURITY.md)。

---

## 1. 推荐技术栈

| 层 | 选择 | 理由 |
|---|---|---|
| 外壳 | MyGo（Go + WebView2）或 Tauri 2 / WinUI 3 | WebView2 是系统组件，包体小 |
| 渲染 | Canvas 2D（JPEG）/ WebCodecs（H.264） | 与浏览器端完全一致 |
| 采集（被控） | DXGI Desktop Duplication，回退 GDI `BitBlt` | 前者 GPU 友好，后者兼容性兜底 |
| 编码（被控） | Media Foundation H.264（硬编），回退 JPEG | 系统自带，覆盖 NVENC/QSV/AMF |
| 输入（被控） | `SendInput` + `SetCursorPos` | 无需驱动；UIPI 限制见 §5 |
| 输入（主控） | Raw Input / 低层钩子（`WH_KEYBOARD_LL`） | 捕获全屏快捷键 |
| 网络 | 复用 `vr-session`（Rust）/ `velarouter-go`（Go） | 已实现握手与加密 |
| 打包 | MSIX（商店）或 NSIS（独立分发） | 签名与更新策略不同 |

如果被控端直接使用本仓库的 Rust 实现，**不需要任何 Windows 专有代码**：
`vr-agent` 已经通过 `xcap`（DXGI/GDI）与 `enigo`（SendInput）完成采集与注入。

---

## 2. 被控端：采集

### 2.1 DXGI Desktop Duplication（首选）

```cpp
// 伪代码：核心调用序列
IDXGIOutputDuplication* dup;
IDXGIOutput1* output1;
output->QueryInterface(__uuidof(IDXGIOutput1), (void**)&output1);
output1->DuplicateOutput(device, &dup);

DXGI_OUTDUPL_FRAME_INFO info;
IDXGIResource* res;
dup->AcquireNextFrame(16, &info, &res);      // 16ms 超时 ≈ 60fps
ID3D11Texture2D* tex;
res->QueryInterface(__uuidof(ID3D11Texture2D), (void**)&tex);
// 复制到 staging texture → Map → 拿到 BGRA 像素
dup->ReleaseFrame();
```

要点：

- 首次调用前必须 `ReleaseFrame` 一次（历史坑）；
- 只有桌面变化时才返回新帧，天然省流；
- 独占全屏游戏（Flip Model）可能返回 `DXGI_ERROR_ACCESS_LOST`，需要重新初始化；
- 多显卡（集显 + 独显）时，用输出所在的 adapter；
- HDR 显示器返回 `DXGI_FORMAT_R16G16B16A16_FLOAT`，需要 tone-map 到 SDR 或走 HDR 通道。

### 2.2 GDI 回退

```cpp
HDC screen = GetDC(nullptr);
HDC mem = CreateCompatibleDC(screen);
HBITMAP bmp = CreateCompatibleBitmap(screen, w, h);
SelectObject(mem, bmp);
BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT);
```

- 兼容性最好，但 **不包含硬件加速窗口**（视频播放、浏览器 GPU 合成可能黑块）；
- `CAPTUREBLT` 会捕获分层窗口，但更慢；
- 仅在 DXGI 不可用时使用。

### 2.3 多显示器

用 `EnumDisplayMonitors` 枚举，每个显示器独立 `IDXGIOutput`。
坐标是虚拟桌面坐标（可为负），协议里 `display` 用稳定 id，`x/y` 用归一化坐标。

### 2.4 缩放与 DPI

- 进程声明 Per-Monitor V2 DPI 感知：

```xml
<dpiAwareness>PerMonitorV2</dpiAwareness>
```

- 物理像素与逻辑像素分离：采集用物理像素，注入用物理像素；
- 混合 DPI（150% + 100%）时，每个显示器的 `scale` 单独计算。

---

## 3. 被控端：编码

### 3.1 Media Foundation H.264

```
MFCreateSinkWriterFromURL / MFCreateVideoEncoder (MFT)
→ IMFTransform::ProcessInput / ProcessOutput
→ CODECAPI_AVEncCommonRateControlMode (CBR/VBR)
→ CODECAPI_AVEncVideoEncodeQP / AVEncCommonQuality
→ 输出 Annex-B 或 AVCC（协议要求 Annex-B）
```

关键参数：

| 参数 | 建议 |
|---|---|
| Profile | High / Main（Baseline 仅兼容旧设备） |
| Level | 4.1（1080p30）/ 5.1（4K） |
| 码率 | 4–8 Mbps（1080p30），由 `video_config.quality` 映射 |
| 关键帧间隔 | 2 秒，或丢包时强制 IDR |
| 低延迟 | 关闭 B 帧：`CODECAPI_AVEncMPVDefaultBPictureCount = 0` |
| 输出 | `MF_MT_MPEG2_STREAM_HEADER` 关闭，手动拼 SPS/PPS |

### 3.2 硬件编码器选择

`StreamerCodecDetector.exe` 这类探测是必要的：

1. 枚举 `MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE)`；
2. 优先 NVENC → QSV → AMF → Media Foundation 软件；
3. 记录驱动版本，已知问题机型强制软件编码（UUYC 的 `force_software_decoder` 就是干这个的）。

### 3.3 回退 JPEG

`VideoHeader.codec = 1`，`quality` 直接映射到 JPEG 质量。
在没有硬编 / 远程办公场景足够，CPU 占用约为 H.264 的 2–3 倍。

---

## 4. 被控端：输入注入

### 4.1 鼠标

```cpp
SetCursorPos(x, y);                    // 绝对坐标（虚拟桌面）
INPUT in = {};
in.type = INPUT_MOUSE;
in.mi.dwFlags = MOUSEEVENTF_LEFTDOWN;  // 或 RIGHT/MIDDLE/UP
SendInput(1, &in, sizeof(INPUT));
// 滚轮：MOUSEEVENTF_WHEEL，mouseData = ±120 * detents
```

要点：

- 绝对坐标用 `SetCursorPos` 比 `MOUSEEVENTF_ABSOLUTE` 更可靠（后者是 0..65535 归一化，易受多屏影响）；
- 多显示器负坐标可以直接传给 `SetCursorPos`；
- 高 DPI 下无需手动缩放（Per-Monitor V2 已处理）。

### 4.2 键盘

```cpp
INPUT in = {};
in.type = INPUT_KEYBOARD;
in.ki.wVk = vk;                        // 虚拟键码
in.ki.wScan = MapVirtualKey(vk, MAPVK_VK_TO_VSC);
in.ki.dwFlags = KEYEVENTF_SCANCODE | (up ? KEYEVENTF_KEYUP : 0);
in.ki.dwFlags |= extended ? KEYEVENTF_EXTENDEDKEY : 0;
SendInput(1, &in, sizeof(INPUT));
```

W3C `code` → VK 映射：

| code | VK |
|---|---|
| `KeyA`..`KeyZ` | `0x41`..`0x5A` |
| `Digit0`..`Digit9` | `0x30`..`0x39` |
| `Enter` | `VK_RETURN` |
| `ShiftLeft/Right` | `VK_LSHIFT`/`VK_RSHIFT` |
| `ControlLeft/Right` | `VK_LCONTROL`/`VK_RCONTROL` |
| `AltLeft/Right` | `VK_LMENU`/`VK_RMENU` |
| `MetaLeft/Right` | `VK_LWIN`/`VK_RWIN` |
| `Arrow*` | `VK_LEFT/UP/RIGHT/DOWN`（扩展键） |
| `F1`..`F12` | `VK_F1`..`VK_F12` |
| `Numpad0`..`9` | `VK_NUMPAD0`..`9` |

### 4.3 UIPI / UAC 限制

- 普通进程**不能**向管理员权限窗口发送输入（UIPI）；
- UAC 安全桌面（Secure Desktop）**完全无法**注入；
- 解决：被控端以管理员身份运行，或安装服务 + 提权 helper（UUYC 用 `GameViewerService.exe`）；
- 但服务运行在 Session 0，**没有用户桌面**，因此必须“服务 + 用户态进程”配合：
  服务负责提权与保活，用户态进程负责采集/注入。

### 4.4 快捷键冲突

主控端捕获 `Ctrl+Alt+Del` 需要 `SendSAS`（需服务权限）；
`Win+L` 等系统快捷键无法拦截，只能提示用户。
建议在主控端提供“发送 Ctrl+Alt+Del”按钮，通过 `SendSAS(FALSE)` 实现。

---

## 5. 主控端：输入捕获

| 场景 | 方案 |
|---|---|
| 窗口内 | 普通 `keydown`/`mousemove` |
| 全屏 + 拦截系统键 | 低层钩子 `WH_KEYBOARD_LL` / `WH_MOUSE_LL` |
| 游戏手柄 | XInput / GameInput |
| 多显示器 | 记录窗口所在显示器的坐标原点 |

注意：低层钩子运行在 UI 线程，回调必须 < 1ms，否则会被系统摘除；
把事件推入无锁队列，由网络线程发送。

---

## 6. 打包与签名

### 6.1 MSIX（商店 / 企业分发）

```xml
<Applications>
  <Application Id="Velarouter" Executable="velarouter.exe" EntryPoint="Windows.FullTrustApplication">
    <uap:VisualElements DisplayName="Velarouter" .../>
    <Extensions>
      <uap:Extension Category="windows.protocol">
        <uap:Protocol Name="velarouter"/>
      </uap:Extension>
    </Extensions>
  </Application>
</Applications>
```

- 需要代码签名证书（EV 证书可减少 SmartScreen 警告）；
- MSIX 不支持“服务”，被控端需要单独分发或使用 `windows.service` 扩展（受限）。

### 6.2 NSIS / Inno Setup（独立分发）

- 安装到 `%ProgramFiles%\Velarouter`；
- 注册开机自启（计划任务，用户登录时运行，**不要**用服务）；
- 安装时申请防火墙规则：

```powershell
netsh advfirewall firewall add rule name="Velarouter" dir=in action=allow program="%ProgramFiles%\Velarouter\velarouter.exe" enable=yes
```

### 6.3 签名

```powershell
signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 `
  /a "Velarouter.exe"
```

未签名的 exe 会被 SmartScreen 拦截；企业分发建议 EV 证书 + 固定发布者名称。

---

## 7. 客户端接入步骤

以 C# / WinUI 3 为例（Go/Rust 同理）：

1. **连接**

```csharp
var ws = new ClientWebSocket();
await ws.ConnectAsync(new Uri($"ws://{host}:34789/vr?sid={sid}"), ct);
await SendText(ws, new { t = "hello", version = 1, role = "viewer",
                         session_id = sid, app = "velarouter-win/0.1" });
```

2. **握手**：见 [`../PROTOCOL.md`](../PROTOCOL.md) §3；用 `ECDiffieHellmanCng`（P-256）+ `HKDF` + `HMACSHA256`。
3. **加密**：`AesGcm`（.NET 6+），nonce = `direction || 000000 || counter`。
4. **接收循环**：解密 → `kind` → JSON 或媒体帧。
5. **渲染**：JPEG 用 `BitmapDecoder` → `SoftwareBitmap` → `Image`/`Canvas`；
   H.264 用 `MediaFoundation`/`MFTransform` 或把 Annex-B 包进 `MediaStreamSource`。
6. **输入**：DOM/UI 事件归一化后发 `InputEvent`。

### 7.1 C# 关键代码

```csharp
// 解密一帧
static byte[] Open(AesGcm aes, ulong counter, ReadOnlySpan<byte> wire)
{
    var iv = new byte[12];
    iv[0] = 2; // agent -> viewer
    BinaryPrimitives.WriteUInt64BigEndian(iv.AsSpan(4), counter);
    var ct = wire[8..];
    var plain = new byte[ct.Length - 16];
    aes.Decrypt(iv, ct[..^16], ct[^16..], plain);
    return plain;
}
```

### 7.2 帧解析

```csharp
var span = plain.AsSpan();
if (span[0] == 2) {
    var frame = span[1..];
    var width  = BinaryPrimitives.ReadUInt16BigEndian(frame[16..]);
    var height = BinaryPrimitives.ReadUInt16BigEndian(frame[18..]);
    var len    = BinaryPrimitives.ReadUInt32BigEndian(frame[20..]);
    var jpeg   = frame.Slice(24, (int)len);
}
```

---

## 8. 测试清单

- [ ] 100% / 150% / 200% DPI 下坐标正确
- [ ] 多显示器（含负坐标、竖屏）切换正确
- [ ] 独占全屏游戏（DXGI access lost 恢复）
- [ ] HDR 显示器不偏色
- [ ] 中文输入法（`Imm32`/TSF）提交文本正确（用 `InputEvent::Text`）
- [ ] `Ctrl+Alt+Del` 通过按钮发送
- [ ] 普通权限被控端无法操作管理员窗口（预期行为，需文档说明）
- [ ] 休眠唤醒后采集自动恢复
- [ ] 断网 30 秒重连
- [ ] 签名后无 SmartScreen 警告

---

## 9. 已知坑

| 坑 | 现象 | 解决 |
|---|---|---|
| DXGI 首次 `AcquireNextFrame` 失败 | 启动即黑屏 | 先 `ReleaseFrame` 再重试 |
| 硬件加速 GPU 计划 | 采集黑屏/花屏 | 关闭该选项或改用 WGC |
| 全屏独占 | `DXGI_ERROR_ACCESS_LOST` | 重新创建 duplication |
| 服务在 Session 0 | 采集到空桌面 | 用户态进程采集，服务只保活 |
| 低层钩子超时 | 系统摘除钩子 | 回调内只入队 |
| 多屏负坐标 | 鼠标偏移 | 用虚拟桌面坐标，别用主屏相对坐标 |
| 输入法 | 字符重复/丢失 | 组合输入用 `Text` 事件，不要逐个 `Key` |
| WebView2 缺失 | 主控端白屏 | 安装 Evergreen Runtime 或固定版本 |
