# 发布核验记录

## v0.1.4 · 2026-11-26 (UTC)

- 源码：`f62b9547d0c1787c0bf35a8ea69da925eebaf0c8`。`v0.1.2` 与 `v0.1.3` 是未公开候选，标签保留；没有覆盖旧发布。
- [主分支 Checks](https://github.com/logdns/Velarouter/actions/runs/45872369632) 与 [标签流水线](https://github.com/logdns/Velarouter/actions/runs/45873181733) 通过。Linux 默认 Rust 98 项通过、试验构建 99 项通过，均无失败；四路 Xvfb 和合成 PulseAudio 独立通过。Go 测试与 vet、Web 测试、Rust Clippy 已过。
- RustSec 未报已知漏洞，仍有 `ttf-parser`、`zbus_names`、`zvariant`、`zvariant_derive` 四项未维护警告。Go、npm 漏洞检查未报漏洞；发布提交快照的 Gitleaks 扫描未报泄漏。这不是第三方安全认证。
- [同一提交的四平台构建](https://github.com/logdns/Velarouter/actions/runs/45872370622) 生成了正式发布资产：macOS ARM64 DMG、Windows x64 ZIP、Linux amd64 和 arm64 的 DEB 与便携包。检查了 Windows ZIP、macOS DMG 与临时签名、Linux 包内程序和菜单资源。从 GitHub 下载六个草稿资产，根据 `SHA256SUMS.txt` 逐项通过后公开。默认包不含 H.264 或 Linux 音频。
- [公开版本](https://github.com/logdns/Velarouter/releases/tag/v0.1.4) 已设置为最新版。[Pages 部署](https://github.com/logdns/Velarouter/actions/runs/45872369622) 通过，核验了 [公开主页](https://2sb.org/Velarouter/)、静态图和下载入口。没有变更线上信令服务。
- 未代替现场验收：真实 Wayland 授权、iOS Safari / Android Chrome、四台实机并发和跨 NAT。执行方法见 [Linux 验收清单](../clients/LINUX-ACCEPTANCE.md)。
