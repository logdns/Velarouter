# 功能地图：UUYC 有什么，Velarouter 做到哪

> 左列来自对 UUYC 4.48.x（Windows / macOS / Linux / Android / TV）的静态分析；
> 右列是 Velarouter 当前代码的真实状态，不是计划。
> 状态：✅ 已实现并验证 · 🚧 部分实现 · 📋 已设计未实现 · ⛔ 明确不做

---

## 1. 远程桌面核心

| 能力 | UUYC 证据 | Velarouter | 代码 / 说明 |
|---|---|---|---|
| 屏幕采集 | Desktop Duplication / ScreenCaptureKit / X11 / MediaProjection | ✅ | `vr-agent/src/capture.rs`（`xcap`） |
| 软件 JPEG | libturbojpeg | ✅ | `image` crate，q20–95，感知哈希跳帧 |
| H.264/H.265/AV1 | `StreamerCodecType`、VideoToolbox/MF/VAAPI | 📋 | 协议已支持 `Codec`，见 `OPEN-SOURCE.md` §2 |
| 帧率档位 30/60/90/144 | `FPS_*` 枚举 | 🚧 | 协议 `fps` 最大 120，采集线程按值节流 |
| 色度 4:2:0/4:2:2/4:4:4 | `ChromaFormat` | 📋 | 属于编码器能力 |
| HDR | `screen_hdr` 能力位 | 📋 | 需要 10-bit 编码链路 |
| 多显示器 | 能力位 + `ActiveDisplay` | 🚧 | ✅ 列表/切换（Web UI 下拉）；并发多路 📋 |
| 光标形状/可见性 | `CursorShapeNotify` | 🚧 | 协议有 `cursor`，agent 暂未采集 |
| 隐私屏 | `screen_privacy` | 📋 | 见 `OPEN-SOURCE.md` §8 |
| 画面批注（Draw*） | `DrawStroke/Clear/Toggle` | 📋 | 客户端叠加层，无需改动媒体链路 |
| 静态画面省流 | （推测） | ✅ | 16×16 感知哈希，相同即跳过 |
| 自适应码率 | QoS + `switchIceNetwork` | 📋 | 当前固定 quality，见 roadmap |

## 2. 输入

| 能力 | UUYC | Velarouter | 说明 |
|---|---|---|---|
| 鼠标/键盘/滚轮 | 原生注入 | ✅ | `enigo`，W3C `code` 映射 |
| 触摸/多点 | `MultiTouchEvent` | ✅ | `InputEvent::Touch`，桌面映射为鼠标 |
| 文本/输入法 | `Ime` 消息 | ✅ | `InputEvent::Text` |
| 剪辑键/组合键 | 虚拟按键方案 | 📋 | 客户端编排，协议无需改 |
| 游戏手柄 | `device_gamepad` | 📋 | 映射为 `Key` 或扩展 `InputEvent` |
| 失焦释放按键 | — | ✅ | `ReleaseAll`，Web 端 `blur` 触发 |
| 坐标归一化 | 像素/DPR 多套 | ✅ | 0..65535，分辨率无关 |

## 3. 剪贴板与文件

| 能力 | UUYC | Velarouter | 说明 |
|---|---|---|---|
| 文本剪贴板双向 | v1/v2/v3 三代 | ✅ | 方向守卫防回环，>1 MiB 拒绝 |
| 文件拖拽/复制 | `FTP2` + FUSE | 🚧 | 文本/路径可用；系统级文件拖拽 📋 |
| 文件上传 | `FileTransferSendRequest` + Block | ✅ | `velarouter put`，SHA-256 校验 |
| 文件下载 | `FileTransferReceiveRequest` | ✅ | `velarouter get`，断点参数已支持 |
| 目录浏览 | `ReadDir` + 压缩列表 | ✅ | `velarouter ls`，目录优先排序 |
| 断点续传 | `FF_FILE_TRANSFER_FTP2_RESUMABLE` | ✅ | `offset` + 前缀重哈希 |
| 临时文件 | `.downloading` | ✅ | `.<name>.velarouter-part` |
| 结果码 | 13 个 `FT_*` | ✅ | 字符串 code（可读优先） |
| FUSE 挂载 | `libfuse3` | 📋 | 跨平台成本高，见 `OPEN-SOURCE.md` §4 |

## 4. 终端

| 能力 | UUYC | Velarouter | 说明 |
|---|---|---|---|
| 远端 shell | `uuyc-cli term -d DEVICE new` | 📋 | 设计见 `OPEN-SOURCE.md` §5 |
| 会话复用 | tmux / uuyc-mux | 📋 | 复用 tmux 或 `portable-pty` |
| 断线重连 | tmux attach | 📋 | 会话存活在 agent |
| 本地终端 | `lterm` | ⛔ | 操作系统的终端已经够好 |
| Windows ConPTY | `conpty_bridge` | 📋 | `portable-pty` 已封装 |

## 5. 网络与传输

| 能力 | UUYC | Velarouter | 说明 |
|---|---|---|---|
| 内网直连 | ✅ | ✅ | `ws://ip:34789/vr`，零服务器 |
| 局域网发现 | — | ✅ | UDP 广播 `VRDISC1` |
| UDP 打洞 | ICE 全打洞 | ✅ | `vr-p2p`：会合 + 打洞 + QUIC，见 `P2P.md` |
| 互联网直连（无中继） | ID + 验证码 | ✅ | 联机编码 + 密码；失败即报错 |
| 中继 | 自建 TURN | ✅ | 密文 WS 管道 |
| 信令多活 | `signalServers[]` | 🚧 | 单节点，客户端可配多个 URL |
| SNI 白名单 | `sniRequiredServers` | 📋 | 反代层实现 |
| UDP 媒体 | SRTP | 📋 | 见 `OPEN-SOURCE.md` §1 |
| 拥塞控制/FEC | WebRTC | 📋 | 同上 |
| 音频 | Opus | 📋 | 见 `OPEN-SOURCE.md` §3 |

## 6. 会话与安全

| 能力 | UUYC | Velarouter | 说明 |
|---|---|---|---|
| 端到端加密 | DTLS-SRTP | ✅ | P-256 + HKDF + AES-GCM，中继不可读 |
| 配对码 | 房间 token | ✅ | 6 位，短时，进 HKDF salt |
| 双向身份证明 | — | ✅ | agent 先证明，防离线爆破 |
| 防重放 | SRTP | ✅ | 严格计数器 |
| 设备身份（TOFU） | 账号绑定 | 📋 | 见 roadmap v0.4 |
| 会话录制 | — | 📋 | 可基于 `FrameSource` 旁路 |
| 并发会话上限 | 会员限制 | 📋 | 需要 `Semaphore` + 配额 |
| 审计日志 | 有 | 🚧 | `tracing` 结构化日志已就绪 |

## 7. 平台与运维

| 能力 | UUYC | Velarouter | 说明 |
|---|---|---|---|
| Windows 服务 | GameViewerService | 📋 | 当前单进程；文档给出 NSSM/计划任务方案 |
| macOS TCC | 屏幕录制 + 辅助功能 | ✅ | `doctor` 检测；文档给出引导 |
| Linux Wayland | PipeWire | ✅ | `xcap` 采集；输入走 RemoteDesktop portal，用户授权后控制所选屏幕，拒绝则只读 |
| 开机自启 | 各平台 | ✅ | systemd/LaunchAgent/计划任务 文档 |
| 自动更新 | Upgrade/Updater | 📋 | MyGo updater / Sparkle / NSIS |
| 崩溃上报 | Sentry + 自研 | ⛔ | 隐私优先，默认关闭 |
| 静默升级 | `TerminalUpgrade` | ⛔ | 安全风险，不做 |

## 8. 生态功能（明确不做）

| 能力 | 原因 |
|---|---|
| 云电脑编排 / 排队 / 计费 | 属于 IaaS 业务，不是远程桌面框架 |
| MuMu 模拟器控制 | 游戏场景插件，核心无关 |
| 会员/活动/支付 | 商业化，不进入开源框架 |
| 游戏反作弊对抗 | 合规风险 |
| 静默监控 | 违反“被控端显式授权”原则 |

---

## 9. 功能借用原则

1. **只借协议与状态机，不抄实现**：UUYC 的 FTP2 状态机值得学，但字段、编码、错误码全部重新设计。
2. **平台能力显式声明**：每个功能都写清“在哪个平台可行、缺什么权限”，不做虚假承诺。
3. **安全默认更严格**：UUYC 的本地控制 socket 是 0666；Velarouter 不暴露本地 socket。UUYC 允许任意下载落点；Velarouter 限制在共享根目录。
4. **可被第三方实现**：新增的每条消息都进 `docs/PROTOCOL.md`，并配套 Go/JS 参考实现。

---

## 更新（v0.1 桌面版）

| 能力 | 状态 | 说明 |
|---|---|---|
| 打开即生成互联网联机码 | ✅ | 12 位，持久保存；临时密码每次启动轮换 |
| 无需部署服务器的跨网连接 | ✅ | 公共 MQTT-TLS 会合 + UDP 打洞 + QUIC，无中继 |
| 局域网码免网络 | ✅ | `VR-…` 直接编码地址，TCP 直连 |
| 深色/浅色自适应界面 | ✅ | Linux 默认浅色，macOS / Windows 默认跟随系统；可在设置中修改，窄窗用页签切换 |
| 键鼠可用性诊断 | ✅ | 未授权时明确标注“仅观看”，不再静默失败 |
| 显示器选择与画质切换 | ✅ | 会话内实时生效 |
| 文件传输面板 | ✅ | 浏览/下载/上传，SHA-256 校验，下载到所选本地目录 |
| 2K / 4K 分辨率切换 | ✅ | 会话内切换，被控端即时重启采集线程 |
| 开机自启 / 后台运行 / 彻底退出 | ✅ | 设置 → 设备，默认开机自启（打包版） |
