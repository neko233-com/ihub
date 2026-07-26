# iHub

<p align="center">
  <strong>你的本地命令空间。</strong><br />
  更快地查找、启动和扩展 Windows / macOS 上的一切。
</p>

<p align="center">
  <img alt="Tauri v2" src="https://img.shields.io/badge/Tauri-v2-24C8DB?logo=tauri&logoColor=white" />
  <img alt="Vite 8" src="https://img.shields.io/badge/Vite-v8-646CFF?logo=vite&logoColor=white" />
  <img alt="React 19" src="https://img.shields.io/badge/React-19-149ECA?logo=react&logoColor=white" />
  <img alt="TypeScript" src="https://img.shields.io/badge/TypeScript-7-3178C6?logo=typescript&logoColor=white" />
</p>

iHub 是一个由 Rust 核心驱动的桌面启动器、本地搜索和插件宿主。它的交互目标是像命令面板一样安静、即时；它的扩展目标则是比传统启动器更开放：插件 UI 用 TypeScript 编写，重能力可以通过独立二进制 worker（包括 ffmpeg）实现。

> 当前仓库是可运行 MVP：包含本地文件名索引、模糊搜索、全局唤起、系统托盘、开机自启、更新配置、GitHub 插件导入底座、插件 SDK 与官方插件注册表。全盘 NTFS USN / macOS FSEvents 的原生索引适配器已留出架构位置，见 [搜索引擎设计](docs/SEARCH_ENGINE.md)。

## 现在能做什么

- Rust 后台并行扫描用户常用目录；搜索过程不在 WebView 中访问文件系统。
- 在一个键盘优先的 React 命令面板中搜索文件、文件夹、命令和插件。
- 通过 Tauri v2 的单实例、托盘、全局快捷键与开机自启能力保持随叫随到。
- 从 GitHub URL 或 <code>github:owner/repo</code> 导入插件。安装器固定来源、读取 manifest，不执行仓库的 npm、Git hook、PowerShell 或 shell 脚本。
- 为插件提供 TypeScript SDK、manifest schema、stdio JSON-RPC worker 协议和 Hello 模板。
- 配置签名自动更新的发布管线；Windows 与 macOS 各自产出原生包。

## 架构

~~~mermaid
flowchart TB
  UI["React + TypeScript<br/>Vite 8 · Motion · React Bits interaction"]
  CORE["iHub Core (Rust)<br/>Search · index · lifecycle · trust"]
  FS["Native file index<br/>parallel scan → persisted index"]
  PUI["Plugin UI (TypeScript)<br/>iHub Bridge only"]
  WORKER["Plugin worker<br/>Binary / ffmpeg / Rust / Go / Python"]
  GH["GitHub repository / release<br/>manifest · hash · signature"]

  UI <--> CORE
  CORE <--> FS
  PUI <-->|"scoped host RPC"| CORE
  CORE <-->|"newline-delimited JSON-RPC"| WORKER
  GH -->|"pinned source + verification"| CORE
~~~

第三方前端绝不直接获得 Tauri IPC 或全盘文件权限；二进制 worker 按需启动，并明确显示风险。用户要求“无沙箱”并不等于没有安全边界：它意味着二进制等价于用户手动运行一个程序，因此 iHub 采用来源锁定、哈希、签名链、权限审阅和可回滚版本来降低供应链风险，而不虚假承诺隔离。

## 快速开始

### 开发环境

需要 Node 22.12+、pnpm、Rust stable 与对应平台的 Tauri 前置依赖。

~~~sh
pnpm install
pnpm tauri:dev
~~~

仅预览 Web UI：

~~~sh
pnpm dev
~~~

### 构建

~~~sh
pnpm build
pnpm tauri:build
~~~

Windows 的开发包可以在 Windows 上构建。macOS <code>.app</code>、签名与公证必须在 macOS runner 或 Mac 上构建。

### 安装发行版

发布后可用以下脚本（默认目标为 <code>neko233-com/ihub</code>）：

~~~powershell
irm https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.ps1 | iex
~~~

~~~sh
curl -fsSL https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.sh | sh
~~~

安装脚本只下载 GitHub Release 资产，并在存在 <code>SHA256SUMS.txt</code> 时进行校验。生产使用前请阅读 [发布与更新](docs/RELEASE.md)。

## 插件：去中心化，但不轻率

用户可以直接输入 GitHub 仓库；官方 catalog 只是发现入口，绝不是唯一下载中心。当前 MVP 会 clone 一个 GitHub 源码仓库、固定 HEAD commit、只识别已构建的前端/二进制文件，绝不执行仓库脚本。生产级的 <code>.ihubpkg</code> Release 资产校验（integrity 清单与发布者签名）已在规范中定义，是下一阶段安装器的目标。

~~~text
GitHub source → pinned commit/tag → manifest / integrity / signature check
              → immutable local store → consent → on-demand worker
~~~

插件包的核心文件为 <code>ihub.plugin.json</code>，声明：

- TypeScript UI 的入口和贡献的命令；
- 每个 OS / CPU 对应的原生 worker；
- 需要的 iHub Bridge 权限；
- 发布者公钥、hash 与更新源。

完整规范、风险模型、协议与开发模板位于：

- [插件架构](docs/PLUGIN_ARCHITECTURE.md)
- [开发第一个插件](docs/PLUGIN_DEVELOPMENT.md)
- [TypeScript SDK](plugin-sdk)
- [Hello 插件](examples/ihub-plugin-hello)

### 首批官方插件

| 仓库 | 主要能力 | 状态 |
| --- | --- | --- |
| [ihub-plugin-ocr](https://github.com/neko233-com/ihub-plugin-ocr) | 截图、剪贴板、图片 OCR | 规划 |
| [ihub-plugin-translate](https://github.com/neko233-com/ihub-plugin-translate) | 划词、剪贴板与多服务商翻译 | 规划 |
| [ihub-plugin-colorpick](https://github.com/neko233-com/ihub-plugin-colorpick) | 全局吸管、颜色转换、历史 | 规划 |
| ihub-plugin-clipboard | 剪贴板历史、固定片段、隐私排除 | 推荐 |
| ihub-plugin-batch-rename | 批量重命名、正则预览、可撤销日志 | 推荐 |
| ihub-plugin-image-tools | 压缩、转换、水印、拼图 | 推荐 |
| ihub-plugin-screen-record | 屏幕录制与 ffmpeg 管线 | 推荐 |
| ihub-plugin-qrcode | 二维码 / 条码生成识别 | 推荐 |
| ihub-plugin-devtools | JSON、Base64、时间戳、hash、正则 | 推荐 |
| ihub-plugin-quick-note | 快速 Markdown 笔记与检索 | 推荐 |

官方插件以独立 Git 仓库维护，主仓库使用 <code>plugins/official</code> 中的映射和 lock file 做集成验证；终端用户安装的是 Release 包，而不是运行 Git 仓库中的安装脚本。

## 搜索引擎路线

“Everything 级速度”不能靠递归扫描冒充。iHub 的进化路径是：

1. **MVP**：Rust 线程池并行扫描、内存文件名索引、增量重建。
2. **持久化与内容**：SQLite / Tantivy 元数据与全文索引；内容提取限额并交给 OCR/PDF/Office 插件。
3. **Windows 加速器**：NTFS MFT / USN Journal 枚举与增量变更；不支持的卷自动降级。
4. **macOS 加速器**：FSEvents 变更流、可靠的全量重扫回退，并将 Spotlight 作为可选辅助。

详细约束、数据模型和基准策略在 [搜索引擎设计](docs/SEARCH_ENGINE.md)。

## 自动更新与开机自启

- 开机自启为显式用户设置，不在首次运行时静默开启。
- 更新使用 Tauri 官方 updater，生产构建必须提供签名公钥、HTTPS endpoint 和私钥环境变量。
- Windows 建议采用用户范围的 passive NSIS 安装；macOS 需要 Developer ID 签名和 notarization。
- GitHub Actions 在 Windows 和 macOS runner 上构建；发布 secrets 不会写入仓库。

## 视觉系统

界面采用低干扰的深石墨命令面板：一个高对比搜索平面、一种青绿强调色、紧凑信息密度与键盘优先流。React Bits 的 BlurText 交互以本地可调组件形式集成，Motion 负责进入、列表和抽屉过渡；所有动画尊重 <code>prefers-reduced-motion</code>。

## 项目结构

~~~text
src/                 React command workspace
src-tauri/           Tauri app + Rust search/plugin runtime
plugin-sdk/          @ihub/plugin-sdk TypeScript package
examples/            Plugin authoring templates
plugins/             Official registry, lock and submodule mapping
scripts/             Windows/macOS installer scripts
docs/                Architecture, search, plugin, release documents
~~~

## 验证

~~~sh
pnpm check
pnpm build
cargo check --manifest-path src-tauri/Cargo.toml
~~~

## 许可证

[MIT](LICENSE)
