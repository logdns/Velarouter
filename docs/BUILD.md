# 构建与发布

## 验证

使用 Rust 1.95+、Go 1.27.4+、Node.js 22+，另需安装 `cargo-audit` 和 `govulncheck`。Linux 先装下方系统依赖。

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets
cargo audit
(cd sdk/go && go test ./... && go vet ./... && govulncheck ./...)
(cd webviewer && npm ci && npm test && npm audit)
```

工作区有一项会实际移动鼠标的测试，默认忽略。图形权限、双机连接和跨 NAT 验收见 [Linux 清单](clients/LINUX-ACCEPTANCE.md)。依赖审计不等于协议安全审计。

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

Linux 包为动态链接程序。无法启动时，用 `ldd ./Velarouter` 检查缺失库，并对照包内 `libraries.txt`。

## 发布与网站

- 推送与桌面 Cargo 版本一致的标签（如 `v0.1.0`）会触发 `desktop` 工作流。构建和测试成功后将生成 **草稿发布**及 SHA-256；核对安装包，再公开草稿。
- 从 [Releases](https://github.com/logdns/Velarouter/releases/latest) 下载四个桌面包和 `SHA256SUMS.txt`，在同一目录运行 `shasum -a 256 -c SHA256SUMS.txt`（Linux 也可用 `sha256sum -c SHA256SUMS.txt`）。
- `site/` 是静态项目主页，不托管远程桌面服务。在本地运行 `python3 -m http.server --directory site` 后可预览；推送到 `main` 会通过 GitHub Actions 发布。线上地址以[仓库 Pages 设置](https://github.com/logdns/Velarouter/settings/pages)为准。
