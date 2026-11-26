# 终端只读预览

`velarouter-view` 在支持 ANSI 的交互式终端中用字符显示远端桌面。它独立构建，不依赖桌面 GUI；本地键鼠不会发送到远端，不同步剪贴板，不传文件，不播放声音，也不会保存画面。

从 0.1.4 起随标准安装包分发：Linux DEB 可直接运行 `velarouter-view`，便携包在解压目录使用 `./velarouter-view`；macOS 在 `/Applications/Velarouter.app/Contents/MacOS/`；Windows 在 ZIP 的 `tools` 目录。`velarouter` 运维工具位于同一目录。

也可单独从源码构建：

```sh
cargo build --release --locked -p vr-terminal
./target/release/velarouter-view '<对方的联机码>'
```

Windows 运行 `target\release\velarouter-view.exe`。输入对方当前的连接密码（输入不回显，不放在命令参数里），按 `Ctrl+C` 退出。

- 同一局域网用原有 `VR-…` 联机码，无需信令服务器。12 位互联网联机码仍走原有会合与 QUIC 直连，失败不自动中继。
- 自建信令分配的 9 位码需要 `--signal https://你的信令地址`；自建会合服务可通过 `--broker` 选用。
- 被控电脑仍需运行 Velarouter 并授权屏幕采集。终端版目前不支持可信设备身份验证；被控端开启「仅允许可信设备连接」时，请用已配对的桌面客户端。
- 字符预览适合观察整体画面变化；查看小字或操作远端请使用桌面客户端。当前没有 Kitty、Sixel、缩放和平移。

终端预览的产品方向参考 [CrossDesk](../FEATURES.md#参考与许可)，实现独立，沿用 Velarouter 的 VRP/1。
