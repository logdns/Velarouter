# 构建与发布

## 验证

使用 Rust 1.95+、Go 1.27.4+、Node.js 22+，另需安装 `cargo-audit` 和 `govulncheck`。Linux 先装下方系统依赖。

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo audit
(cd sdk/go && go test ./... && go vet ./... && govulncheck ./...)
(cd webviewer && npm ci && npm test && npm audit)
```

会实际移动鼠标的测试和需要独立虚拟设备的测试默认忽略。Linux CI 另外运行下列隔离测试及启用 H.264、Linux 音频的工作区测试：

```sh
sudo apt-get install xvfb xauth x11-xserver-utils pulseaudio pulseaudio-utils
bash scripts/test-desktop-wall.sh
bash scripts/test-linux-audio.sh
cargo test --workspace --locked --features vr-desktop/h264,vr-desktop/linux-audio
```

桌面墙测试在私有 Xvfb 上使用四个回环监听器，验证四路已配对连接都收到 JPEG 画面，以及移除、退出后释放会话；不读取真实桌面，也不模拟跨 NAT。图形权限、双机连接和跨 NAT 验收见 [Linux 清单](clients/LINUX-ACCEPTANCE.md)。依赖审计不等于协议安全审计。

## 打包

产物在 `dist/`，不会提交到 Git。

- **macOS Apple Silicon**：在本机运行 `bash scripts/package-macos.sh`。需要 Xcode 或命令行工具。DMG 内 App 为临时签名，未公证。
- **Windows x64**：在 Windows PowerShell 运行 `powershell -File packaging/windows/build.ps1`。生成便携 ZIP；安装了 NSIS 时也可本地生成安装器。

### Linux

在目标架构的 Debian 12 / Ubuntu 24.04 上运行：

```sh
sudo bash scripts/linux-build-deps.sh
bash scripts/package-linux.sh
```

也可用 Buildx 构建 Debian 12 兼容包：

```sh
docker buildx build --platform linux/amd64 -f packaging/linux/Dockerfile.cross-amd64 --output type=local,dest=dist .
docker buildx build --platform linux/arm64 -f packaging/linux/Dockerfile --output type=local,dest=dist .
```

每种架构生成 `.deb`（Ubuntu / Debian，注册应用菜单和图标）及 `.tar.gz`（便携安装）。Linux 仍是动态链接程序；如无法启动，用 `ldd` 检查缺失库，并对照便携包的 `libraries.txt`。

## H.264 软件编解码（试验）

```sh
cargo build --release --locked -p vr-desktop --features h264
cargo test --workspace --locked --features vr-desktop/h264
```

两台电脑都使用该构建时才会请求 H.264；仍用原联机码和密码。网页、终端和默认发布包保持 JPEG。当前软件后端最多 1080p / 30 fps，不含硬编；Linux 音频需另外启用下方开关。使用源代码构建 Cisco OpenH264，需要本地 C/C++ 工具链。分发启用此功能的二进制前，应分别核对上游许可、专利与分发要求。

## Linux 系统声音（试验）

在 Linux 桌面安装 PulseAudio 客户端工具（Debian / Ubuntu：`pulseaudio-utils`），运行 PulseAudio 或 PipeWire 的 PulseAudio 兼容服务。两端都使用下列构建：

```sh
cargo build --release --locked -p vr-desktop --features linux-audio
```

H.264 与声音可独立启用。可选后端验收：安装 `pulseaudio` 后，以普通用户运行 `bash scripts/test-linux-audio.sh`，只使用独立空输出设备和合成测试声音。默认安装包不包含该试验功能。

## 发布与网站

- 推送与桌面 Cargo 版本一致的标签（如 `v0.1.2`）会触发 `desktop` 工作流。构建和测试成功后将生成 **草稿发布**及 SHA-256；核对安装包，再公开草稿。
- 从 [Releases](https://github.com/logdns/Velarouter/releases/latest) 下载安装包和 `SHA256SUMS.txt`，在同一目录运行 `shasum -a 256 -c SHA256SUMS.txt`（Linux 也可用 `sha256sum -c SHA256SUMS.txt`）。
- 安装包同时包含 `velarouter` 运维工具和 `velarouter-view` 终端只读客户端；位置见[终端指南](clients/TERMINAL.md)。
- `site/` 是静态项目主页，不托管远程桌面服务。在本地运行 `python3 -m http.server --directory site` 后可预览；推送到 `main` 会通过 GitHub Actions 发布。线上地址以[仓库 Pages 设置](https://github.com/logdns/Velarouter/settings/pages)为准。
