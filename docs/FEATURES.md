# 功能清单

Velarouter 是**局域网优先、无需账号的加密远程桌面**。同一局域网用联机码和密码直接连接，不依赖云端；跨网默认通过会合服务建立 QUIC 点对点连接，失败不会偷偷切换中继。桌面、内置 Web 主控和 CLI 延续现有 VRP/1，不为追求功能对齐而替换协议。

以下是 **0.1.4 的交付范围**与待评估方向。下载状态以 [GitHub Releases](https://github.com/logdns/Velarouter/releases/latest) 为准，平台能力取决于系统权限与运行环境。

| 能力 | 现在可用 | 值得借鉴 / 暂不承诺 |
|---|---|---|
| 跨平台远控 | Windows x64、macOS Apple Silicon、Linux x86_64 / ARM64 均可主控与被控；内置局域网 Web 主控支持窄屏和单指操作 | iOS / Android 原生端仍是候选，不承诺手机被控 |
| 局域网直连 | 桌面用 `VR-…` 码直连；最近连接按局域网／互联网分组，不保存密码 | 保留原有联机码输入，不增加裸地址输入 |
| 可信设备 | 可选名单限制，同时校验密码和主控设备签名；默认关闭 | 不自动信任历史连接；旧版、Web 和 CLI 不能连接已开启限制的被控端 |
| 终端主控 | CLI 支持诊断和文件传输；[ANSI 只读预览](clients/TERMINAL.md)随 0.1.4 安装包分发 | Kitty / Sixel、缩放和平移待评估；不含交互式远控或远端 shell |
| 桌面墙 | 按组预览最多四台、放大单个画面；每台最高 480×270 / 3 fps，需两端升级 | 实机与跨网并发待验收；不含群控和批量分发 |
| 画面与声音 | JPEG 基线；会话内可选显示器、调画质、帧率和传输画面尺寸；暂无远端音频 | 开发版可用构建开关试验 H.264 软编解码；只有两端都支持才启用，其他客户端仍用 JPEG。开发版另有 Linux 系统声音试验（两端主动开启、不采麦克风）；AV1、HEVC 与平台硬编仍待评估。调传输尺寸不等于修改远端系统显示模式，后者单独评估 |
| 日常协作 | 键鼠、文本剪贴板、显示器切换、双栏文件浏览、文件／目录收发、拖放上传与队列；Wayland 键鼠需要用户授权 | 优先补连接历史与排障，不重做现有能力。网页与原生客户端不默认等价 |
| Agent / MCP | 未实现；项目里的 `velarouter agent` 是被控端进程，并非 AI Agent 集成 | 仅评估本机、显式授权的只读预览与人工接管；首版不开放自动键鼠、命令、剪贴板或文件操作 |
| 部署与运维 | 支持自建信令与加密 WebSocket 中继接口；默认跨网走 QUIC 直连，桌面不会自动回落 | 是否增加显式可选中继，由跨网验收决定；不把 TURN／SRTP 写成现有能力。Windows 登录前远控、Linux 虚拟桌面／无头被控单独评估 |

## 保留 Velarouter 的特点

- **不换连接入口。** 保留现有局域网码、互联网联机码与临时密码；桌面输入框仍只接受原有联机码，生成与保存方式不变。
- **不引入必需的云服务。** 局域网断网仍应可用；跨网直连与中继要能看出区别，自托管继续可选。
- **不以功能清单代替验收。** Windows、macOS、Linux 的权限和驱动限制分别记录；浏览器不是原生移动端，抓帧 CLI 不是终端远控，开机自启也不是登录前控制。

## 参考与许可

最近设备、可信设备、终端预览、桌面墙等**产品方向**参考了 [CrossDesk 中文 README（`mcp` 分支快照）](https://github.com/kunkundi/crossdesk/blob/cea7cfafa9de514c5df4cc6b5ffa7d4e736f27f9/README_ZH.md)。最近连接分组、手动配对、终端只读预览与桌面墙均借鉴产品思路并独立实现；没有移植其代码、协议、界面素材或依赖。实验性 H.264 后端直接使用 [OpenH264 Rust 绑定（BSD-2-Clause）](https://github.com/ralfbiedert/openh264-rs)与 [Cisco OpenH264](https://github.com/cisco/openh264)，不来自 CrossDesk。CrossDesk 该分支为 [GPL-3.0-only](https://github.com/kunkundi/crossdesk/blob/cea7cfafa9de514c5df4cc6b5ffa7d4e736f27f9/LICENSE)，Velarouter 维持 [Apache-2.0](../LICENSE)；后续如实际引入代码或资源，应先评估许可，再在对应实现和发布说明中标注来源。

下一步见[路线图](ROADMAP.md)。
