# 客户端开发文档索引

五个平台的能力差异不是“实现程度”问题，而是**操作系统允许你做什么**。
先看这张表，再读对应平台的文档。

---

## 1. 能力矩阵

| 能力 | Windows | macOS | Linux (X11) | Linux (Wayland) | iOS | Android |
|---|---|---|---|---|---|---|
| 做主控端（看+控） | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 做被控端（共享屏幕） | ✅ | ✅ | ✅ | ⚠️ Portal | ✅ ReplayKit | ✅ MediaProjection |
| 被控端可注入输入 | ✅ | ✅ | ✅ | ⚠️ libei | ❌ 系统禁止 | ⚠️ 无障碍/输入法 |
| 硬件编码 | MF/NVENC/QSV/AMF | VideoToolbox | VAAPI/NVENC | 同左 | 无（只读共享） | MediaCodec |
| 多显示器 | ✅ | ✅ | ✅ | ⚠️ | — | ⚠️ |
| 剪贴板同步 | ✅ | ✅ | ✅ | ⚠️ | ✅（主控） | ✅ |
| 后台常驻 | 服务+用户进程 | LaunchAgent | systemd user | 同左 | ❌ | 前台服务 |
| 商店分发 | MSIX | App Store / 公证 | Flatpak/deb/rpm | 同左 | App Store | Google Play |

图例：✅ 完整；⚠️ 有条件；❌ 不可行；— 不适用。

---

## 2. 文档

| 平台 | 文档 | 关键约束 |
|---|---|---|---|
| 桌面客户端（三端） | [DESKTOP-APP.md](DESKTOP-APP.md) | egui 初版；macOS/Windows 已构建 |
|---|---|---|
| Windows | [windows.md](windows.md) | UIPI/UAC、Session 0、DXGI 失效恢复 |
| macOS | [macos.md](macos.md) | 屏幕录制 + 辅助功能双 TCC、公证 |
| Linux | [linux.md](linux.md) | X11 vs Wayland 是两条产品线 |
| iOS | [ios.md](ios.md) | **只能做主控端**；被控仅 ReplayKit 只读 |
| Android | [android.md](android.md) | 无障碍注入 + 前台服务 + Play 政策 |

---

## 3. 共同部分

所有平台共用同一套协议与步骤，先读：

1. [`../PROTOCOL.md`](../PROTOCOL.md) — 线格式、握手、加密、消息目录
2. [`../SIGNALING.md`](../SIGNALING.md) — 设备注册、配对、会话、中继
3. [`../SECURITY.md`](../SECURITY.md) — 权限与威胁模型
4. [`../sdk/README.md`](../sdk/README.md) — Rust / JS / Go 三套参考实现

---

## 4. 选型建议

| 目标 | 推荐 |
|---|---|
| 五端统一、包体小、团队会 Go | MyGo + `sdk/go`（见 [`../MYGO.md`](../MYGO.md)） |
| 只要桌面、团队会 Rust | Tauri 2 + 直接复用 `vr-session` |
| 深入系统、要极致性能 | 各平台原生（WinUI / AppKit / GTK4 / SwiftUI / Compose） |
| 零安装、临时支持 | 浏览器打开被控端的 `http://<ip>:34789/` |
| 企业批量部署 | MSIX / PKG / deb + MDM，被控端走 systemd/LaunchAgent/计划任务 |

---

## 5. 验收基线（所有平台）

- [ ] 通过 `docs/PROTOCOL.md` §3.4 的已知答案向量
- [ ] 错误配对码被拒绝，且提示不泄露细节
- [ ] 分辨率变化后坐标仍正确（归一化）
- [ ] 断网 30 秒能自动重连
- [ ] 60 秒无数据会主动 ping
- [ ] 失焦发送 `release_all`，不留卡住的按键
- [ ] 解码不排队（最多 1 帧在飞）
- [ ] 权限被拒绝时有明确引导，而不是黑屏
- [ ] 会话结束后密钥与计数器清零
