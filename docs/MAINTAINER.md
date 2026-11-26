# 本地维护与发布闭环

## 每轮开发

1. 从[功能清单](FEATURES.md)与[路线图](ROADMAP.md)选一个可验证的范围，列明涉及的客户端、默认行为、权限和跨版本兼容。保留原联机码、VRP/1、局域网离线直连及跨网 QUIC 优先；不增加裸地址输入或静默中继。
2. 实现与回归一起提交。网络失败需能取消、退出并释放会话；重连需用户确认，不保存密码。媒体功能在设备不可用或拒绝时不能中断基础远控。Web、终端、桌面不能互相代替验收。
3. 按[构建指南](BUILD.md)跑默认与试验特性、审计、格式检查；补测试证据。容器通过记为自动化通过，不记为真机通过。
4. 同步 README、[介绍](OVERVIEW.md)、[客户端索引](clients/README.md)和适用的用户手册。若改变下载或部署信息，同步 `site/index.html` 与[部署指南](DEPLOYMENT.md)。

## 发布与回验

1. 核对 `Cargo.toml` 与 `Cargo.lock` 的版本，准备 `docs/releases/vX.Y.Z.md`。只有已验证内容才写进发布说明。签名、实验开关和未验收平台继续明确标注。
2. 推送 `main` 后检查 **Checks** 和 **GitHub Pages**。本地可用 `actionlint`、`shellcheck`；Actions 的审计包含 RustSec、govulncheck、npm audit 和提交快照的密钥扫描。RustSec 的未维护警告要记录并维护替换计划，不能当作第三方安全认证。
3. 检查通过后推送新标签（如 `v0.1.2`）。**Desktop packages** 重新执行检查并原生构建 macOS ARM64、Windows x64、Linux amd64/arm64；不会覆盖已有发布。未打标签时可手动运行仅构建产物。
4. 下载草稿资产核对六个包与 `SHA256SUMS.txt`。macOS 校验 DMG 和 App 签名；Windows 验证 ZIP 和命令行入口；Linux 验证 DEB/便携包的程序、版本、启动项及图标一致。按 [Linux 实机清单](clients/LINUX-ACCEPTANCE.md)记录能完成的现场测试，再公开草稿。
5. 公开后复查 [Releases](https://github.com/logdns/Velarouter/releases/latest)、[Pages](https://2sb.org/Velarouter/) 和下载哈希。回退时优先引导使用已知可用的旧版，不改写已发布标签或替换原有包；修复用新版本。信令部署单独维护，不由站点或桌面发布触发。

## 当前交付范围

- `velarouter-desktop`：桌面主控／被控；`velarouter-view`：终端只读；`velarouter`：运维与诊断。移动浏览器使用被控端的局域网页面。
- 自动化：四路桌面墙 Xvfb、合成 PulseAudio、密码及可信设备校验、共享目录传输、跨语言已知答案向量。Windows／macOS／Linux 安装包测试不替代现场桌面操作验证。
- 尚需真机：异构桌面双向连接，Wayland 授权，iOS Safari 与 Android Chrome，四台实机并发及跨 NAT。H.264 分发许可及专利未评审，Linux 音频仍为试验。AV1、HEVC、硬编、原生移动应用和登录前远控不计入本版完成项。

现场记录：版本与提交｜系统/架构｜测试网络与角色｜动作｜预期｜实际｜脱敏证据。不要记录联机密码、信令令牌、设备私钥或私人画面。
