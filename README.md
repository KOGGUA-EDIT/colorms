# Colorms 2.0

用 Rust + egui 做的 Material You 风格颜色制作器。界面按照 `ui.vectorcraft`
（576 × 576 画板）1:1 还原并随窗口等比缩放。

2.0 把窗口壳和渲染整个换掉了：不再用 eframe / OpenGL，改成自研 CPU 光栅器
+ Windows 分层窗口，冷启动从 ~344 ms 降到 ~65 ms（约 5.2×），详见[性能](#性能)。

![Colorms](docs/screenshot.png)

## 功能

- **圆形色盘**：角度选色相，半径选饱和度；滚轮区域外的角度也继续生效。
- **明度胶囊**：左侧黑白渐变竖条，顶部 100% 白色、底部 0% 黑色。
- **圆形预览**：左下角实时显示最终颜色。
- **十六进制值**：右下胶囊显示 `#RRGGBB`，点击即复制。
- **复制按钮**：右侧双层圆角卡片本身就是复制图标（无内嵌图标/文字）；
  点击复制当前色值，整个卡片从灰色变为白色作为确认，上层卡片会遮住下层重叠部分。
- **动态主题**：背景、复制按钮、描边随当前颜色生成 Material You 暗色配色。
- **无边框自适应窗口**：自定义标题栏 / 最小化 / 最大化 / 关闭，
  6px 边缘拖拽调整大小；Windows 下用**分层窗口**（`WS_EX_LAYERED` +
  `UpdateLayeredWindow`，逐像素 alpha）呈现，圆角因此是真抗锯齿的，
  窗口本身没有原生边框；2px 窗框用预渲染的抗锯齿圆角掩码绘制。
  同时去掉原生标题栏、禁用 DWM 过渡与自带圆角，
  首帧画完才显示窗口，避免白窗闪烁。最底部固定为 `#202020`。

## 快捷键

| 按键 | 作用 |
| --- | --- |
| `Ctrl + C` | 复制当前颜色 |
| `Esc` | 关闭提示气泡 |

## 性能

冷启动按“进程启动 → 首帧可见”优化。同一台机器、同一套外部计时脚本
（枚举窗口直到可见且尺寸正确）交替跑 6 轮：

| 版本 | 中位数 | 范围 |
| --- | --- | --- |
| 1.0（eframe / OpenGL） | 337 ms | 328–396 ms |
| 2.0（CPU 光栅 + 分层窗口） | **65 ms** | 62–67 ms |

约 **5.2×** 的提升，主要来自不再初始化 GPU 驱动：

- 不链接 `eframe` / `glutin` / `egui_glow`，改用 `winit` + `egui-winit` 自建窗口壳，
  跳过 `ChoosePixelFormat` + `wglMakeCurrent`（本机约 210 ms 的 GL ICD 初始化）。
- 自研 CPU 三角形光栅器（`src/gfx.rs`）把 egui 的三角网格画进 BGRA DIB，
  再通过分层窗口（`UpdateLayeredWindow`）提交，全程不碰 GPU。全窗口渐变、环境光晕都走整行常量快路径，
  光晕预烘焙成一张 128² 径向 alpha 贴图（原来的 16 层同心圆叠加合并成一次采样）。
- 启动路径上没有 SQLite：数据库在后台线程打开，窗口先用默认尺寸创建并居中，
  数据就绪后再套用保存的尺寸；任务栏图标也是首帧之后再装（省 ~15 ms）。
- 字体（Segoe UI / 等线 / Consolas）与 768² 色盘纹理在后台线程生成，
  色盘按行分给多个线程并行计算（45 ms → 6 ms），首帧前刚好完成。
- 窗口创建即去掉原生标题栏、禁用 DWM 过渡动画（否则首次 `ShowWindow` 会等 ~240 ms），
  首帧画完才 `set_visible`，避免白窗闪烁。
- 发布版 `lto = "fat"`、`codegen-units = 1`、`panic = "abort"`、`strip = true`，
  可执行文件约 5.1 MB；空闲时不重绘（0% CPU）。

## 技术栈

- GUI：`egui` / `egui-winit` 0.33.3，窗口壳用 `winit` 0.30 + 手写 Win32 FFI（无 eframe）
- 渲染：自研 CPU 三角光栅器（`src/gfx.rs`）+ 分层窗口逐像素 alpha 提交（`src/platform.rs`）
- 存储：`rusqlite` 0.37（bundled SQLite，WAL 模式）
- 序列化：`serde` / `serde_json`
- 错误处理：`thiserror`（数据层）+ `anyhow`（应用层）
- 测试：`cargo test`（21 个单元测试）+ `criterion`（核心色彩路径）

## 构建与运行

需要 Rust 1.88+ 和可用的 C 工具链（`rusqlite` 的 bundled SQLite 需要 `cc`）。
Windows 上可以用 MSVC Build Tools，或者 GNU 目标 + w64devkit
（GNU 目标还需要 w64devkit 的 `dlltool.exe` 在 `PATH` 里）。
当前主要针对 Windows（无边框分层窗口、圆角 alpha、系统字体加载按 Windows 调优）。

```powershell
cargo run            # 开发运行
cargo test           # 单元测试
cargo bench --bench color
cargo build --release
```

生成的可执行文件：

- 调试版：`target\debug\colorms.exe`
- 发布版：`target\release\colorms.exe`

### 启动打点

设置 `COLORMS_TRACE=1` 后，启动各阶段（`main enter` → `first frame visible`）
带毫秒时间戳打到 stderr（先缓冲、一次性输出，避免 1.5 ms/行的 stderr 开销污染测量）。
`COLORMS_GFX_TRACE=1|2|3` 输出每帧/每图元/每三角形耗时，`COLORMS_DUMP=<绝对路径.bmp>`
在首帧后把画布写成 BMP，便于和 `docs/screenshot.png` 对比。

## 数据位置

- 数据库：`%APPDATA%\Colorms\colorms.db`（WAL）

数据库不可用时应用会自动回退到内存数据库，并在窗口内提示。
启动路径上不写日志文件（旧版的 `tracing` 日志已移除，冷启动更快）。

## 目录

```
src/
  main.rs      启动：时钟打点、后台线程（字体/色盘/数据库）、进入事件循环
  platform.rs  winit 窗口壳 + Win32 FFI（去标题栏、分层窗口呈现、DWM、圆角 alpha）+ 重绘调度
  gfx.rs       CPU 三角光栅器：egui 图元 → BGRA 画布（实色/渐变/纹理三条内核）
  trace.rs     启动打点（缓冲后一次性输出）
  app.rs       应用状态：绘制、交互、持久化、toast、Command 队列
  ui.rs        布局/度量、色盘/明度/光晕纹理、图标与背景绘制
  color.rs     色彩模型（HSV / hex / 对比度 / Material 色）
  theme.rs     动态 Material You 暗色主题
  db.rs        SQLite（WAL）、上次颜色与窗口尺寸
benches/
  color.rs     criterion 基准
assets/        窗口图标原始 RGBA
ui.svg / ui.vectorcraft / ui.png
               设计稿
```
