# Linux 客户端开发文档

> 目标：实现符合 VRP/1 的 Linux 主控端与被控端。
> 协议：[`../PROTOCOL.md`](../PROTOCOL.md)。

---

本项目可构建 Linux x86_64 / aarch64 原生客户端，两种架构的打包、下载与运行说明见[构建指南](../BUILD.md#linux)。当前发布物是 tar.gz，不是 AppImage、Flatpak 或系统服务；包内 `install.sh` 负责安装当前用户的应用菜单、Dock 图标和 `.desktop` 项。

## 1. 两条完全不同的路径

Linux 的远程桌面难点不在编码，而在 **显示服务器协议**：

| 会话类型 | 采集 | 输入注入 | 结论 |
|---|---|---|---|
| **X11** | `xcap` → X11 | `enigo` 的 `x11rb` / XTest | ✅ 完整可用，无需额外授权 |
| **Wayland** | `xcap` → libwayshot，失败时 portal 截图 | `xdg-desktop-portal` RemoteDesktop（经用户授权） | ✅ 授权后可控制**所选屏幕**；拒绝或桌面不支持时仅观看 |

当前发布构建：X11 使用 `x11rb` 注入；Wayland 由 `vr-agent/src/portal_input.rs` 通过 `ashpd` 申请 `org.freedesktop.portal.RemoteDesktop`，由用户在系统弹窗中选择屏幕并打开「允许远程交互」。没有授权时**不会回退到 X11 注入**，避免在 Wayland 会话里出现“部分窗口能点、原生应用不能点”的假象。现场验收请按[公网信令与 Linux 双机验收记录](LINUX-ACCEPTANCE.md)分别记录画面和输入。

---

## 2. X11 被控端

### 2.1 采集

```c
Display *dpy = XOpenDisplay(NULL);
Window root = DefaultRootWindow(dpy);
XWindowAttributes attr;
XGetWindowAttributes(dpy, root, &attr);

XImage *img = XGetImage(dpy, root, 0, 0, attr.width, attr.height,
                        AllPlanes, ZPixmap);
// img->data 是 BGRA（小端），需要按 4 字节对齐
XDestroyImage(img);
```

高性能版本用 MIT-SHM：

```c
XShmSegmentInfo shminfo;
XImage *img = XShmCreateImage(dpy, visual, depth, ZPixmap, NULL, &shminfo,
                              width, height);
shminfo.shmid = shmget(IPC_PRIVATE, img->bytes_per_line * img->height,
                       IPC_CREAT | 0600);
shminfo.shmaddr = img->data = shmat(shminfo.shmid, NULL, 0);
XShmAttach(dpy, &shminfo);
XShmGetImage(dpy, root, img, 0, 0, AllPlanes);
```

- `XShmGetImage` 比 `XGetImage` 快 5–10 倍（避免每帧 round-trip）；
- 多屏用 `XineramaQueryScreens` 或 `XRRGetMonitors` 获取各显示器坐标；
- 4K 下每帧 33 MB，必须做缩放，不要直接编码全尺寸。

### 2.2 输入

```c
XTestFakeMotionEvent(dpy, screen, x, y, CurrentTime);
XTestFakeButtonEvent(dpy, 1, True, CurrentTime);   // 1=左 2=中 3=右
XTestFakeKeyEvent(dpy, keycode, True, CurrentTime);
XFlush(dpy);
```

W3C `code` → X11 keysym：

```c
KeySym ks = XStringToKeysym("a");        // 或手写映射表
KeyCode kc = XKeysymToKeycode(dpy, ks);
```

- 修饰键用 `XTestFakeKeyEvent` 单独按下/抬起；
- 滚轮是按钮 4/5（垂直）、6/7（水平）；
- `CurrentTime` 即可，不要用 `XSync`（会引入延迟）。

### 2.3 权限

X11 下通常无特殊权限；但如果启用了 `xhost` 白名单或 SELinux，`XOpenDisplay` 会失败。
Wayland 会话里 `DISPLAY` 可能仍存在（XWayland），但**只能看到 XWayland 窗口**，
看不到原生 Wayland 窗口——这是最常见的“采到黑屏”的原因。

---

## 3. Wayland 被控端

### 3.1 采集：Portal + PipeWire

```c
GDBusConnection *bus = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, NULL);
// 调用 org.freedesktop.portal.ScreenCast
// 1) CreateSession
// 2) SelectSources (types = MONITOR, multiple = true, cursor_mode = EMBEDDED)
// 3) Start → 返回 PipeWire node id
// 4) 用 gst-launch / pipewire 读取帧
```

GStreamer 管道：

```
pipewiresrc path=<node_id> ! videoconvert ! video/x-raw,format=BGRA \
  ! appsink
```

- 必须由**用户**在 portal 对话框里选择屏幕（无法静默采集）；
- 每次会话都会弹窗，这是设计，不是 bug；
- 部分合成器（GNOME 45+）支持“记住选择”，KDE 支持较好；
- 采集到的是合成后的画面，**没有窗口级 API**（`SelectSources(types=WINDOW)` 需 compositor 支持）。

### 3.2 输入：RemoteDesktop portal

发布构建的 Wayland 路径：

1. 主机启动共享（桌面客户端默认在打开时自动开始）时，`portal_input::request` 创建 `RemoteDesktop` 会话；
2. `SelectDevices(POINTER | KEYBOARD)` 申请指针和键盘；
3. `ScreenCast.SelectSources(MONITOR, multiple=false)` 让用户选择屏幕；
4. `RemoteDesktop.Start` 弹出系统的「远程桌面」对话框；用户必须选择屏幕、打开「允许远程交互」，再点「分享」；
5. 只有 `Start` 返回的 `devices` 同时包含指针和键盘，才把授权标记为可用；否则会话按只读处理；
6. 指针坐标按 portal 返回的流尺寸归一化，按键/按钮走 `NotifyKeyboardKeysym` / `NotifyPointerButton`，滚轮走 `NotifyPointerAxisDiscrete`。

行为约束：

- 授权只覆盖用户在系统弹窗里选中的**一块屏幕**；主控端收到的 `DisplayList` 被过滤为这一块，切换到其他屏幕不会注入。
- 用户取消弹窗、关闭「允许远程交互」或系统撤销授权时，`receive_closed` 会结束授权；当前会话立即停止注入，主控端显示「仅观看」。
- 授权状态变化会实时同步到已连接会话；重新申请需要在本机「设置 → 权限」点「重新申请权限（重启本机共享）」，该操作会断开现有会话。
- 不支持 RemoteDesktop 的合成器、没有 `xdg-desktop-portal` 的环境，或只运行在 XWayland 兼容层的会话，按受限记录；不要用 XTest 兜底冒充 Wayland 原生输入。
- `libei` 仍是合成器侧的实现细节；应用侧走 portal，不直接链接 libei。
- 不支持全局快捷键拦截。

### 3.3 屏幕共享的替代方案

如果只需要“共享一个窗口”，可以用：
- `org.freedesktop.portal.ScreenCast` + `SelectSources(types=WINDOW)`;
- 或在应用内自绘（例如共享一个虚拟显示器）。

---

## 4. 编码

| 方案 | 库 | 说明 |
|---|---|---|
| VAAPI | `libva` + `vaExportSurfaceHandle` | Intel/AMD 集显，H.264/HEVC/AV1 |
| NVENC | `libnvidia-encode` | N 卡，延迟最低 |
| Vulkan Video | `VK_KHR_video_encode_h264` | 新卡，生态尚早 |
| x264 | `libx264` | 软件，CPU 高但兼容 |
| JPEG | `libjpeg-turbo` | 基线，本仓库默认 |

FFmpeg 是最省事的统一入口：

```bash
ffmpeg -f x11grab -framerate 30 -video_size 1920x1080 -i :0.0 \
       -c:v h264_vaapi -b:v 6M -g 60 -bf 0 \
       -f h264 pipe:1
```

但**不要**在会话里起 ffmpeg 子进程：进程启动 100ms+、无法精细控制码率与关键帧。
生产应直接链接 `libavcodec` 或各自硬件库。

---

## 5. 主控端

### 5.1 技术选型

- MyGo / Tauri（WebView + Canvas）；
- 或 GTK4 + `GtkGLArea`（原生，性能最好）；
- Wayland 下窗口坐标与缩放由 compositor 决定，注意 `wl_output.scale`。

### 5.2 输入捕获

- X11：`XGrabKeyboard` / `XGrabPointer` 可以独占；
- Wayland：只能拿到应用内焦点，无法全局拦截；
- 游戏手柄：`evdev` / `libudev`（需要 `input` 组权限）。

### 5.3 剪贴板

- X11：`XGetSelectionOwner` + `XFixes` 监听；
- Wayland：`wl_data_device`，只在有焦点时可用；
- 用 `arboard` 可以同时处理两者（本仓库已用）。

---

## 6. 打包与分发

### 6.1 当前产物与可选格式

当前发布物是 `Velarouter-0.1.0-linux-<arch>.tar.gz`：

```sh
./install.sh                 # 安装到 ~/.local/bin、应用菜单和 hicolor 图标
./Velarouter                 # 也可直接从解包目录运行
```

`install.sh` 不写系统目录、不需要 sudo；它把 48/128/256 px 品牌图标和 `.desktop` 项装到当前用户目录。直接运行二进制时窗口图标来自内嵌资源，但应用菜单/Dock 图标需要安装后才能稳定关联。

| 可选格式 | 适用 | 备注 |
|---|---|---|
| AppImage | 通用、免安装 | 体积大，需自带 WebKitGTK |
| `.deb` / `.rpm` | 企业分发 | 依赖系统 WebKitGTK |
| Flatpak | 桌面商店 | 需要 portal 权限声明 |

Flatpak 的 `finish-args`（被控端）：

```yaml
finish-args:
  - --share=network
  - --socket=wayland
  - --socket=pulseaudio
  - --talk-name=org.freedesktop.portal.Desktop
  - --filesystem=xdg-run/pipewire-0
```

Flatpak 沙箱内**无法**直接读 X11 全屏，必须走 portal。

### 6.2 systemd 用户服务

`~/.config/systemd/user/velarouter-agent.service`：

```ini
[Unit]
Description=Velarouter agent
After=graphical-session.target

[Service]
ExecStart=/usr/bin/velarouter agent --signal https://signal.example.com
Restart=always
RestartSec=3

[Install]
WantedBy=graphical-session.target
```

```bash
systemctl --user enable --now velarouter-agent
loginctl enable-linger $USER    # 未登录也运行（Wayland 采集仍然需要会话）
```

注意：**X11/Wayland 采集必须运行在图形会话内**，`enable-linger` 只保证进程存活，
没有 `DISPLAY`/`WAYLAND_DISPLAY` 时依然采不到。

---

## 7. 客户端接入（Rust）

直接依赖本仓库：

```toml
[dependencies]
vr-session = { path = "../velarouter/crates/vr-session" }
vr-proto   = { path = "../velarouter/crates/vr-proto" }
```

```rust
use vr_session::{Session, SessionConfig, WsChannel};
use vr_transport::MessageChannel;
use vr_proto::{Capabilities, Control, Codec, VideoConfig};

let channel = WsChannel::connect("ws://192.168.1.10:34789/vr").await?;
let session = Session::connect(
    Box::new(channel) as Box<dyn MessageChannel>,
    SessionConfig {
        session_id: "s1".into(),
        secret: "123456".into(),
        app: "velarouter-linux/0.1".into(),
        device: None,
        capabilities: Capabilities::default(),
    },
).await?;

let (mut tx, mut rx, _peer) = session.into_parts();
tx.send_control(Control::VideoConfig(VideoConfig::default())).await?;

while let Ok(ev) = rx.recv().await {
    match ev {
        vr_session::Event::Media { header, payload } => { /* decode */ }
        vr_session::Event::Control(Control::Ping { ts }) => {
            tx.send_control(Control::Pong { ts }).await?;
        }
        _ => {}
    }
}
```

---

## 8. 测试清单

- [ ] X11 单屏 / 双屏 / 竖屏
- [ ] 4K + 150% 缩放的坐标正确
- [ ] Wayland（GNOME / KDE）下采集弹窗与画面
- [ ] Wayland 首次启动出现 RemoteDesktop 授权弹窗；拒绝后只能观看且有明确提示
- [ ] Wayland 授权后仅能控制所选屏幕；撤销授权后立即停止注入
- [ ] 应用菜单与 Dock 显示品牌图标（运行 `install.sh` 后）
- [ ] 系统浅色/深色切换后界面跟随（无需重启）
- [ ] 无 `DISPLAY` 时启动给出可读错误
- [ ] 剪贴板在 X11/Wayland 下均可用
- [ ] systemd 用户服务在登录后启动
- [ ] Flatpak 沙箱内 portal 流程
- [ ] 断网重连、休眠唤醒

---

## 9. 已知坑

| 坑 | 现象 | 解决 |
|---|---|---|
| XWayland 假 X11 | 采到黑屏/只有部分窗口 | 检查 `XDG_SESSION_TYPE` |
| portal 每次弹窗 | 用户体验差 | 当前版本每次申请都会弹窗；记录 portal restore token 是后续优化 |
| Wayland 取消授权后仍能点 | 误用 X11 兜底 | 本版本在 Wayland 授权失败时不回退 XTest，检查 `XDG_SESSION_TYPE` 与授权状态 |
| PipeWire 帧格式 | 颜色错乱 | 显式 `videoconvert` 到 BGRA |
| XShm 未对齐 | 图像斜切 | `bytes_per_line` 可能与 `width*4` 不同 |
| 多屏负坐标 | 鼠标偏移 | 用 Xinerama 的完整虚拟桌面坐标 |
| VAAPI 初始化失败 | 退回软件编码卡顿 | 探测 `/dev/dri/renderD128` 权限 |
| Flatpak 沙箱 | 读不到 X11 | 使用 portal，不要 `--socket=x11` 硬闯 |
| systemd 无图形环境 | `DISPLAY` 为空 | 用 `graphical-session.target` 绑定 |
