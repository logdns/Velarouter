# 客户端与平台

| 入口 | 当前范围 | 文档 |
|---|---|---|
| Windows x64 | 原生桌面主控／被控。登录界面、UAC 安全桌面不属于当前支持范围 | [使用指南](DESKTOP-APP.md) · [平台技术参考](windows.md) |
| macOS Apple Silicon | 原生桌面主控／被控。画面需要屏幕录制权限，键鼠需要辅助功能权限 | [使用指南](DESKTOP-APP.md) · [平台技术参考](macos.md) |
| Linux x86_64 / ARM64 | 原生桌面主控／被控。X11 与 Wayland 分别验收；Wayland 输入需要系统 RemoteDesktop 授权 | [使用指南](DESKTOP-APP.md) · [平台技术参考](linux.md) |
| Web | 被控端内置的局域网主控页面。0.1.4 改善手机触屏体验；仅在可信网络使用，不是 GitHub Pages 上的公网主控 | [连接与安全](../SECURITY.md) |
| CLI | 探测、抓帧、文件传输与运维；0.1.4 随安装包提供只读终端预览 | [终端只读预览](TERMINAL.md) · [构建与测试](../BUILD.md) |
| iOS / Android | **尚未发布原生客户端**。以下是平台技术参考，不代表产品已支持 | [iOS](ios.md) · [Android](android.md) |

[功能清单](../FEATURES.md) · [路线图](../ROADMAP.md) · [VRP/1 协议](../PROTOCOL.md) · [Go SDK](../sdk/README.md)
