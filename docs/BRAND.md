# 品牌规范

> Velarouter = **Velocity** + **Router**：把画面快速路由到你面前。
> 视觉语言：深色、冷静、有方向感；一个“跳”连接两个端点。

---

## 1. 标志概念

主标志是一条 **V 形路由**：左端点（青绿）与右端点（蓝）在下方汇合成一个节点，
左侧两道弧线表示“速度”，节点外的圆环表示“信号/一跳”。

```
    ●───────────────●        两个端点
     \             /
      \           /          一条路径（V）
       \         /
        \       /
         ◉                   汇合节点
        ((•))                一跳
```

含义：

- **V**：Velarouter 的首字母，也是“路径”的象形；
- **两个端点**：主控端与被控端；
- **节点**：中继/会合点，但两端仍然端到端加密；
- **弧线**：速度与低延迟；
- **深色底**：工具属性，不抢内容。

---

## 2. 色彩

| 名称 | Hex | 用途 |
|---|---|---|
| `ink-900` | `#0B0D10` | 主背景、深色标志底 |
| `ink-800` | `#14181D` | 卡片、面板 |
| `ink-700` | `#1B2027` | 次级面板、按钮 |
| `line` | `#262D36` | 分隔线、描边 |
| `text` | `#E7ECF3` | 主文字 |
| `muted` | `#8B97A6` | 次级文字 |
| `blue` | `#4F8CFF` | 主品牌色、链接、主按钮 |
| `teal` | `#7EE0C1` | 强调、成功、渐变终点 |
| `cyan` | `#63B8F2` | 渐变中点 |
| `danger` | `#FF5D5D` | 错误 |

品牌渐变（唯一允许的渐变）：

```css
linear-gradient(135deg, #4F8CFF 0%, #63B8F2 55%, #7EE0C1 100%)
```

浅色场景的加深版本（保证对比度）：

```css
linear-gradient(135deg, #3D7BF0 0%, #2FA78A 100%)
```

---

## 3. 字体

| 用途 | 字体 | 字重 | 备注 |
|---|---|---|---|
| 标志字 | Inter / Söhne / SF Pro Display | 700 | 字距 -2% ~ -3% |
| 标题 | Inter / SF Pro / PingFang SC | 600–700 | 中文用系统黑体 |
| 正文 | Inter / system-ui | 400–500 | 行高 1.6 |
| 代码 | JetBrains Mono / SF Mono | 400 | — |

标志的 SVG 使用 `font-family="Inter, 'Helvetica Neue', Helvetica, Arial, sans-serif"`；
正式印刷/商标注册时，应把文字转为路径（Outline）以避免字体依赖。

---

## 4. 素材清单

```
assets/brand/
├── svg/
│   ├── logo-mark.svg               主标志（深色圆角底，1024）
│   ├── logo-mark-transparent.svg   透明底主标志
│   ├── logo-horizontal-dark.svg    横版组合（深色背景用）
│   ├── logo-horizontal-light.svg   横版组合（浅色背景用）
│   ├── logo-stacked.svg            竖版组合 + 标语
│   ├── logo-mono-white.svg         单色白
│   ├── logo-mono-black.svg         单色黑
│   └── favicon.svg                 16–32px 优化
├── png/
│   ├── logo-*.png                  2048px 位图
│   ├── appicon/velarouter-{16..1024}.png
│   └── favicon.png
├── app-icons/
│   ├── macos/Velarouter.icns       macOS（含 iconset 源）
│   ├── windows/velarouter.ico      Windows（16–256，PNG 压缩）
│   ├── ios/AppIcon-*.png           iOS 全尺寸
│   └── android/                    传统 + 自适应前景/背景
├── icon-composer/
│   ├── Velarouter-1024.png         导入用主图
│   └── layers/                     分层：背景 / 标志 / 高光
├── social/
│   └── og-image-1200x630.png       Open Graph / 分享卡
```

所有位图都由 SVG 生成，改标志只需改 SVG 后重跑脚本（见 §9）。Linux 的 `.deb` 以及便携安装脚本都将 48/128/256 px 品牌 PNG 安装到 hicolor 主题，并以 `Icon=Velarouter` 关联与窗口 ID 同名的 `Velarouter.desktop`。

---

## 5. 使用规则

### 清空区

标志四周至少留出 **一个端点圆的直径**（约标志宽度的 7%）作为清空区；
横版组合的清空区以标志高度为基准。

### 最小尺寸

| 场景 | 最小宽度 | 说明 |
|---|---|---|
| 主标志 | 24 px | 小于 24 px 用 `favicon.svg` |
| 横版组合 | 120 px | 小于 120 px 只保留标志 |
| 竖版组合 | 160 px | — |

### 允许

- 在深色/浅色背景上使用对应版本；
- 单色印刷使用 mono 版本；
- 等比缩放；
- 在品牌渐变上叠加白色单色标志（对比度 ≥ 4.5:1）。

### 禁止

- 改变标志比例、旋转、倾斜；
- 给标志加投影、描边、外发光；
- 更换品牌色或使用非指定渐变；
- 把标志放进另一个圆角容器（已有圆角底）；
- 在复杂照片上直接叠加彩色标志（用白色单色版）；
- 用文字“Velarouter”搭配其他图形冒充标志。

---

## 6. 语气与文案

| 场景 | 推荐 | 避免 |
|---|---|---|
| 一句话 | 近在眼前的远程桌面 | 全球领先的远程桌面解决方案 |
| 内网 | 同一个局域网，不绕路 | 无需配置，一键即连（夸张） |
| 公网 | 中继只传密文 | 军用级加密 |
| 开源 | 4000 行核心，可审计 | 完全自主可控 |
| 性能 | 静态画面零带宽 | 零延迟 |

英文标语：**Remote desktop, one hop away.**

中文标语：**近在眼前的远程桌面。**

---

## 7. 用 Icon Composer 生成 macOS 26 图标

Apple 的 Icon Composer 目前只有 GUI，没有命令行。步骤：

1. 打开 **Icon Composer**（Xcode 26 自带，或 `/Applications/Icon Composer.app`）。
2. 新建文档，命名 `Velarouter`。
3. 把 `assets/brand/icon-composer/layers/background.png` 拖入作为底层。
4. 添加图层，拖入 `layers/mark.png`，居中，缩放约 78%。
5. 可选：拖入 `layers/highlight.png`，置顶，混合模式 `Screen`，不透明度 40%。
6. 右侧材质选择 **Glass**（或 Default），圆角半径保持系统默认（不要手动加圆角）。
7. 预览 Dark / Light / Tinted / Clear 四种外观，确认标志在每种下都可读。
8. **File → Export**，选择 `.icon`，保存到 `assets/brand/icon-composer/Velarouter.icon`。
9. 在 Xcode 的 App Icon 槽位选择该 `.icon`；构建时 Xcode 会生成各平台所需尺寸。

> 如果没有 Icon Composer，使用 `app-icons/macos/Velarouter.icns` 作为传统图标。

---

## 8. 导出与验证

```bash
# SVG → PNG（任意尺寸）
rsvg-convert -w 1024 -h 1024 assets/brand/svg/logo-mark.svg -o icon.png

# macOS icns
iconutil -c icns assets/brand/app-icons/macos/Velarouter.iconset \
  -o assets/brand/app-icons/macos/Velarouter.icns

# 检查尺寸
sips -g pixelWidth -g pixelHeight assets/brand/png/appicon/velarouter-1024.png
```

一致性检查：

- [ ] 16 px 下 V 与节点仍可辨认（用 `favicon.svg`，不要缩主标志）；
- [ ] 单色版在黑白打印下对比清晰；
- [ ] 深色/浅色背景各版本对比度 ≥ 4.5:1；
- [ ] iOS 图标无透明通道（App Store 会拒）；
- [ ] Windows ICO 含 16/32/48/256 四档；
- [ ] OG 图 1200×630，文字在安全区内。

---

## 9. 重新生成素材

仓库中的位图由以下命令生成（需要 `rsvg-convert`、`sips`、`iconutil`）：

```bash
cd assets/brand
# 主图与组合
for f in svg/*.svg; do rsvg-convert -w 2048 -o "png/$(basename "${f%.svg}").png" "$f"; done
rsvg-convert -w 1200 -h 630 -o social/og-image-1200x630.png social/og-image.svg

# 应用图标
mkdir -p app-icons/macos/Velarouter.iconset
for s in 16 32 128 256 512; do
  rsvg-convert -w $s -h $s -o app-icons/macos/Velarouter.iconset/icon_${s}x${s}.png svg/logo-mark.svg
  rsvg-convert -w $((s*2)) -h $((s*2)) -o app-icons/macos/Velarouter.iconset/icon_${s}x${s}@2x.png svg/logo-mark.svg
done
iconutil -c icns app-icons/macos/Velarouter.iconset -o app-icons/macos/Velarouter.icns
```

---

## 10. 商标与许可

- 代码许可：Apache-2.0；
- 品牌素材（`assets/brand/`）：随项目分发，允许在“指代本项目”的场景使用；
- 不得将 Velarouter 名称/标志用于暗示 endorsement 或作为自己产品的品牌；
- UUYC / UU 远程 是网易的商标，本项目与其无关联。
