# Velarouter

<img src="assets/brand/png/appicon/velarouter-128.png" alt="Velarouter 标志" width="64" height="64">

局域网优先的加密远程桌面。两台电脑装同一个客户端，用联机码和密码连接，无需注册账号。

**[下载安装](https://github.com/logdns/Velarouter/releases/latest)** · **[项目主页](https://logdns.github.io/Velarouter/)** · **[使用指南](docs/clients/DESKTOP-APP.md)**

| 系统 | 下载后怎么用 |
|---|---|
| macOS（Apple Silicon） | 打开 `.dmg`，将 Velarouter 拖到「应用程序」。 |
| Windows x64 | 解压 ZIP，运行 `Velarouter.exe`。 |
| Linux x86_64 / ARM64 | 按 `uname -m` 选包，解包后运行 `./install.sh` 安装应用菜单。 |

## 怎么连接

1. 在**被控电脑**打开 Velarouter，将「本机共享」里的联机码和密码发给你信任的人。
2. 在**主控电脑**打开「连接电脑」，粘贴两项，点击「连接」。同一局域网优先使用「局域网」里的 `VR-…` 联机码，密码不变。
3. 需要结束访问时，在被控电脑点击「暂停共享」或「停止共享」。看得到但不能操作时，检查「设置 → 权限」。

局域网直连不需要服务器。跨网默认通过公共会合服务交换加密的连接信息，画面和操作走点对点连接；部分网络（如严格 NAT）可能无法直连，默认不会自动中继。桌面端打开即共享，打包版默认启用开机自启，可在「设置 → 设备」关闭自启。

## 开发

需要 Rust 1.95+；Linux 先安装[构建依赖](docs/BUILD.md#linux)。

```sh
cargo test --workspace --locked
cargo run --locked -p vr-desktop
```

[文档索引](docs/README.md) · [构建与发布](docs/BUILD.md) · [安全说明](docs/SECURITY.md)

代码按 [Apache-2.0](LICENSE) 许可发布。品牌使用见[说明](docs/BRAND.md)。
