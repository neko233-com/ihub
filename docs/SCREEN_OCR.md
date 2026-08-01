# 屏幕 OCR

iHub 内置屏幕 OCR 把“截图、框选、语言、识别结果”放在同一个受信任工作台中。它与可安装 OCR 插件是两条不同通道：内置工作台可以在用户点击后调用原生显示器截图与 Windows OCR；普通插件仍然拿不到截图像素或本机路径。

## 工作流

1. 用户点击“截取主显示器”，iHub 先隐藏自己的窗口，再原生读取一帧 PNG 并立即恢复窗口。
2. 用户在预览中拖拽矩形选区。裁剪在 WebView 画布中完成，确认前不写入磁盘。
3. 工作台读取 `Windows.Media.Ocr.OcrEngine.AvailableRecognizerLanguages`，可以使用当前用户首选语言自动识别，也可以指定一个已安装语言包。
4. 选区以受限 PNG data URL 交给主窗口专属 Tauri 命令。Rust 解码并复核格式、字节数和像素数，再通过 `InMemoryRandomAccessStream` 交给 `BitmapDecoder` 和 `OcrEngine`。
5. 识别结果最多 256 KiB，在 UTF-8 字符边界截断；图片、结果和 Blob URL 都只保留在当前页面状态，关闭工作台即释放。

## 上限与降采样

- PNG 最大 16 MiB，最大 2400 万像素。
- Windows OCR 自己公开一个 `MaxImageDimension` 单边上限。工作台会先读取当前系统值；若选区超过该值，会在本机画布中保持宽高比等比缩小，再识别。
- 同一时间只允许一个 OCR 任务。
- 本地图片入口只接受用户明确选择的 PNG/JPEG；JPEG 会在内存画布中转换为 PNG。

## 隐私与平台状态

整个识别过程没有 HTTP 请求、云端回退、临时文件、外部 OCR worker、命令行进程或后台监听。当前运行验收范围是 Windows 10/11 x64；Windows 必须安装至少一个 OCR 语言包。浏览器开发预览只显示确定性模拟画面和选区交互，不会伪造识别结果。

自动化覆盖包括 PNG/data URL 边界、语言标签、UTF-8 截断、等比缩放、主窗口命令 ACL、真实 Windows 内存 OCR 管线，以及包含可见 `iHub`/`OCR` 文字的本机识别冒烟测试。
