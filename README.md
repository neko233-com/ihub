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

> 当前仓库是可运行 MVP：包含本地文件名索引、真实系统应用发现、模糊搜索、全局唤起、系统托盘、开机自启、更新配置、GitHub 插件导入与显式刷新、内置工具箱、插件 SDK 与官方插件注册表。已授权目录会通过系统文件监听进行去抖增量同步；队列溢出或根目录边界变化才会安全重扫当前范围。Windows 另有只在用户**明确授权盘符根目录**时可用的只读 NTFS USN P1a/P1c 加速。P1d/P1e 仅在 iHub 状态目录不位于任何授权盘符根目录内、MFT 初始化完整且 watcher 已登记时启用：Journal 未变时直接复用快照；有有限、连续变更时以只读 USN 回放稳定路径绑定，并在多卷回放和原子写入前双重验证。未知 FRN、硬链接变更、重解析点、重命名不完整、Journal 间断、同卷状态写入或范围不确定都会回退完整扫描。窄目录不会扩大为整卷读取，macOS FSEvents 水位持久化仍是后续升级，见 [搜索引擎设计](docs/SEARCH_ENGINE.md)。

## 现在能做什么

- Rust 后台并行扫描用户常用目录；搜索过程不在 WebView 中访问文件系统。
- Windows 只读扫描当前用户与所有用户的 Start Menu 快捷方式，macOS 只读扫描系统与用户 Applications/PreferencePanes 中的 `.app`、`.prefPane`；它们以真实“应用”结果与文件、文件夹、命令和插件并列，只有显式选择才会启动。Windows 会为当前原生搜索结果（普通文件／目录、`.exe`、`.lnk`、`.url`）和已固定启动项异步读取 48px Shell 原生图标：WebView 只能提交当前结果 ID 或不透明的固定项 ID，宿主解析真实路径后由专用 STA worker 优先读取 `.lnk` 的 `IconLocation`／目标或 `.url` 的 `IconFile`／`IconIndex`，再回退到 Shell 提取。macOS 当前只为 `.app`、`.prefPane` 应用包读取 PNG-backed ICNS 并归一化为 48px，普通文件与目录仍显示中性宿主占位。文字搜索不等待图标，提取失败也不会用伪造的通用 EXE 图标冒充原图。
- **文件启动**：在搜索结果中右键真实、可固定的本地文件、文件夹或应用即可固定到启动页“已固定”。最多保存 18 个此类快捷项；路径和索引来源只保存在 iHub 原生 app-data，前端只拿不透明 ID。打开时 Rust 会从当前索引重新解析、检查类型/授权范围并 canonicalize；失效、链接重定向或不受支持的目标会安全报错而不会猜测替代目标。Windows Start Menu 的 `.lnk/.url` 仍可即时搜索打开，但不会作为持久启动项固定。
- 通过 Tauri v2 的单实例、托盘、全局快捷键与开机自启能力保持随叫随到。默认使用 `Alt+Space`（macOS 为 `Option+Space`）：后台时居中唤出，已显示且聚焦时再次按下隐藏；可见但失焦时只恢复焦点并保留当前查询/工具表面。原生层会抑制按键自动重复；托盘与第二实例始终只显示，不会误触反向隐藏。设置页可直接录制跨平台自定义组合；若启动时被其他应用占用，会明确显示备用键和“重新尝试 Alt + Space”，占用者退出后无需重启即可取回默认键。标题栏关闭、Esc 和失焦只隐藏；托盘或设置页的“退出 iHub”才会结束驻留进程并释放全局键。
- 设置中可显式启用仅支持 Windows / macOS 的 dTools 风格“超级面板”：在其他应用里静止长按右键 460ms，物理位移超过 10px 即取消，iHub 会在指针附近、当前显示器工作区内打开同一个 800×380 紧凑启动器；macOS 需要为 iHub 开启 Input Monitoring。宿主只读观察右键按下／移动／释放，不拦截或合成输入；一次只保留一个 8 秒、仅可消费一次的随机 token，消费后才读取**当前剪贴板**，最多返回 32 个文件 metadata、经尺寸／像素／内存／12MiB PNG 上限重新编码的一张图片，或 4KiB 文本。事件队列、线程启动／停止等待和所有载荷都有硬上限，上下文与 token 只驻留内存且不写历史；关闭功能会停止系统监听并撤销待用 token。监听器在启动或恢复时失败会回退并持久化为 disabled，而不会留下假开启状态。
- 启动器会在一次动作成功打开或交付后更新本地自适应顺序：“最近使用”按 14 天半衰期的频率／时效权重排列，搜索结果只获得最高 480 分的有界加成；这项加成本身不能跨越 exact／prefix 之间的 1,000 分相关性层级。排序账本最多 256 项、保留 180 天，只在本机保存计数、时间戳和由原始结果 ID 映射出的 `usage-v1` 128-bit 伪匿名键；这份账本不保存路径、查询、剪贴板内容或插件 payload。
- 偏好设置内置可滚动的 Rust 宿主诊断：按 256 KiB 单文件、最多 4 个文件轮转，界面最多读取最新 1000 条；桌面端在上一次读取完成 3 秒后串行刷新、默认跟随最新记录，并可手动刷新、复制或清空。清空会先使旧读取失效并等待它结束，旧快照不会在清空提示后回流。常见敏感字段赋值、Authorization、URL user-info、JWT 形态与绝对路径会在落盘前做模式化脱敏；宿主不主动记录剪贴板内容、插件 `details`、launcher context、命令输入、stdout 或 stderr，插件自由文本仍不得包含用户内容或凭据。浏览器预览只显示固定安全 fixture；安装器和开发 watcher 在应用未启动时继续使用各自的有界外部状态／日志，详见[开发机运行与更新](docs/DEVELOPMENT.md#应用内滚动诊断日志)。
- 从 GitHub URL、<code>github:owner/repo@tag</code> 或 <code>owner/repo@tag</code> 导入插件。完整 URL 必须是无 user-info、无 query 的公开 HTTPS 地址，导入器不会接收私有仓库凭据；旧版 source lock 在进入 IPC 或再次交给 Git 前也会重新验证，含凭据的记录只会要求重新导入而不会回显原始 URL。安装器先解析远端 ref，再锁定实际 commit，且不执行仓库的 npm、Git hook、PowerShell 或 shell 脚本。
- 为插件提供自包含 TypeScript iframe bridge、manifest schema、stdio JSON-RPC worker 协议和前端 + Rust worker 模板。
- 可见插件可从宿主标题栏或 `Ctrl+D` 分离为原生 800×600 可调整窗口；新窗口只加载 iHub 同源、可信 React host，真实插件仍位于独立 loopback iframe 与同一受限 Bridge 中。可信 host 的本地 capability 只有 7 条专用 custom-command ACL（bootstrap、close、frontend URL、一次取色批准、release、touch、Bridge call）和 event listen/unlisten；loopback iframe 本身没有任何 Tauri capability。原生注册表把精确 `pluginId` 绑定到唯一窗口 label 与活动 lease，命令、搜索选择和前端快捷键只用 `emit_to` 发给该 owner；搜索选择还必须消费原生端保存的短期、一次性 issued snapshot。关闭时由原生窗口事件兜底释放 lease；分离 host 不获得 Electron、Node、任意 URL 或直接 Tauri shell capability，插件声明的 `shell.*` 仍只能经过同一受限 Bridge。
- 已接入 Tauri 签名 updater 的客户端配置与发布管线；Windows 与 macOS 各自产出原生包。生产更新仍需实际私钥、签名／公证凭据和已发布的 HTTPS `latest.json`。

### 内置工具（现在可用）

- **本地搜索**：主命令框由 Rust 索引驱动；可在“工具 → 本地搜索”查看、添加或移除索引目录，保存后后台重扫。名称与路径使用 Unicode NFKC 检索键，中文文件名和路径支持无声调全拼、首字母、中文／英文混输与未完成音节（例如 `zhongwenjihua` / `zwjh` → “中文计划”）。多音字按字典中的常见读音保证召回，当前不做词组上下文消歧；原文和直接英文名称优先排序。索引只保存两组有界字符签名并共享一份拼音词典，不保存逐条拼音字符串，也不改写显示路径或磁盘快照。
- **颜色工具**：独立三栏工作台提供可拖动与方向键微调的色相/饱和度色轮、明度与透明度、互补/类似/分裂互补/三角色，以及 HEX、RGB、HSV/HSB、HSL、CMYK、CIE-LAB、OKLCH、CSS 一键复制；内置 Apple 高饱和功能色卡和本机收藏。桌面端还有短时限频的原生 9×9 光标放大取色，支持 EyeDropper 的 WebView 也可直接调用系统吸管。
- **截图**：每次通过系统选择器明确选择屏幕、窗口或标签页，导出 PNG 并保留当前会话预览。
- **剪贴板历史**：默认关闭；只有手动开启后才在本机保存纯文本，可固定、复制、删除和清除未固定记录。
- **JSON**：独立双栏编辑工作台，本地识别 JSON、URL Params、XML 与 YAML；支持格式化、压缩、转义、转 XML、转 TypeScript，以及不执行脚本的受限 JSONPath 查询。输入不会发送到网络。
- **离线翻译**：独立双栏工作台默认随应用提供中英双向基础包，自动识别中英文并选择另一侧目标语言；输入、结果和词典匹配全部在 WebView 内完成，状态栏明确显示“网络请求 0”。其他语言可导入最多 1 MiB / 5,000 词条的纯 JSON 本地语言包，单机最多保留 8 个、总量 2 MiB；包不会执行代码，也没有云端回退。词典覆盖率和未覆盖片段会如实显示，格式见[离线翻译语言包](docs/OFFLINE_TRANSLATION.md)。
- **速记、转换与计算器**：本机便签的保存、搜索、复制、删除，以及 BigInt 二/八/十/十六进制、UTF-8 Hex / Base64 转换和离线四则/括号/幂表达式计算；计算器历史只保存在本机。
- **二维码**：离线将文本或 URL 编码为二维码，预览后导出 PNG；也可识别你主动选择的本地图片。图片不会上传、不会读取相册或调用摄像头。
- **统一云盘（WebDAV）**：第一方原生连接器，只在首次连接时接收密码；认证成功后浏览、下载和上传只使用随机原生会话 ID。可选择只连接一次，也可明确保存到 Windows 凭据管理器 / macOS 钥匙串，普通元数据文件和插件桥均拿不到密码。连接器拒绝重定向与非本机 HTTP；下载使用原生保存框和不覆盖临时文件，上传使用原生选择器、唯一暂存名与 `MOVE + Overwrite: F`。阿里云盘、百度网盘与 OneDrive 预留相同 UI 下的独立 OAuth 原生适配器，不会把令牌交给前端。
- **录屏**：独立三栏录屏工作台会把显示器、窗口或浏览器标签偏好交给系统选择器，支持 24/30/60 FPS 质量档、可选系统音频、暂停/继续、来源结束自动保存、WebM 预览与再次下载；单次活跃录制最多 30 分钟或 512 MiB。MP4/FFmpeg、全局快捷键、按键显示与点击高亮必须由单独审核的原生插件承载，内置录屏不会记录键盘输入。
- **批量重命名**：只操作选定目录的直接普通文件；必须先预览，应用阶段会再校验路径、符号链接、冲突和过期预览。
- **插件项目创建器**：在指定绝对父目录生成一个独立的 TypeScript + Vite 前端、Rust JSONL worker、Windows/macOS 构建脚本与协议文档；目标目录已存在时绝不覆盖，也不会自动运行脚本。

## 架构

~~~mermaid
flowchart TB
  UI["React + TypeScript<br/>Vite 8 · Motion · React Bits interaction"]
  CORE["iHub Core (Rust)<br/>Search · index · lifecycle · trust"]
  FS["Native file index<br/>parallel scan → persisted index"]
  PUI["Plugin UI (TypeScript)<br/>独立 loopback 来源 + iHub Bridge"]
  WORKER["Plugin worker<br/>Binary / ffmpeg / Rust / Go / Python"]
  GH["GitHub repository / release<br/>manifest · source/integrity lock"]

  UI <--> CORE
  CORE <--> FS
  PUI <-->|"scoped host RPC"| CORE
  CORE <-->|"newline-delimited JSON-RPC"| WORKER
  GH -->|"pinned source + verification"| CORE
~~~

每个 TypeScript 插件 iframe 都使用独立、短生命周期的 `127.0.0.1` 来源，并只读取声明入口所在的专用构建目录；它不使用 Tauri `asset:` 协议或直接 `invoke`。同一插件只保留一个当前租约，父窗口以精确的 iframe `source`、`origin` 和宿主签发的租约验证 Bridge 消息；更新、停用、解除链接或异常 renderer 超时会使旧租约失效，因此前端只能通过声明的宿主 Bridge 请求能力。未声明非空 `network.allow` 时，iframe CSP 会禁止外部 connect、image 和 media；一旦存在非空声明，当前执行边界只粗粒度开放 HTTPS/WSS connect 与 HTTPS image/media，声明的 destination 会进入安装审计、source lock 和更新比较，但尚不是逐 origin 的运行时网络过滤器。跨 origin 插件也不会继承宿主的屏幕或麦克风能力：只有 Rust 验证为可见 `Surface`、活动 lease 且清单分别声明 `screenCapture: true` / `microphone: true` 时，可信 host 才为该 iframe 分别委派 `display-capture` / `microphone`；隐藏搜索 runtime 和未声明插件没有委派。这仍不能跳过浏览器／系统选择器或 OS 权限，浏览器布局 QA 也不能证明真机权限、实际画面或音频采集成功。用户要求“无沙箱”仍然适用于原生 worker：含二进制的插件必须按本机代码信任，只导入你审阅并信任的发布者。iHub 会把来源、请求 ref、实际 commit、manifest、完整前端资产目录、声明图标和原生二进制的 SHA-256 写入安装锁，并在加载、执行、检查更新与应用更新前复核；发布者签名、界面中的逐字段权限／哈希 diff、第三方静默自动更新与回滚仍是下一阶段能力，不能被当作已经实现。

## 快速开始

### 开发环境

需要 Node 22.12+（含 Corepack）、Rust stable、Git 与对应平台的 Tauri 前置依赖。

开发机推荐直接运行当前工作树，而不是安装一份容易落后的副本：

~~~powershell
# Windows
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\dev.ps1
~~~

~~~sh
# macOS
bash ./scripts/dev.sh
~~~

脚本会以锁文件同步依赖、进行 TypeScript 检查，然后启动 Tauri 开发端；保存源码后，重载的就是当前工作树中的最新代码。想让开发机每次启动时尽可能追随远端，运行 `./scripts/dev.ps1 -UpdateIfClean`（Windows）或 `bash ./scripts/dev.sh --update-if-clean`（macOS）：它只会在工作树干净且可严格 fast-forward 时更新，其他情况保留当前源码继续启动，绝不 reset、checkout、clean 或覆盖工作区。若希望无法更新时明确失败，改用严格的 `-Update` / `--update`。完整的本机开发、构建和安装说明见[开发机运行与更新](docs/DEVELOPMENT.md)。

若需要让**已安装的开发副本**跟随本地保存源码，显式运行 `./scripts/dev.ps1 -WatchInstall`（Windows）或 `bash ./scripts/dev.sh --watch-install`（macOS）；它们不更新 Git、不启动或结束 iHub，只在稳定源码变更后走当前工作树的安全打包/安装路径。Windows 还可运行 `./scripts/install-dev.ps1`，在用户目录和开始菜单创建“iHub Development (Always Latest, Safe)”“Current Source”“Update & Launch”“Install Current Build”和“Watch & Install Current Build”入口；第一个入口会在安全条件满足时自动 fast-forward，条件不满足则直接运行当前保存源码。若明确需要登录后持续运行“安全同步 + 关闭 iHub 后安装”的开发服务，先创建启动器，再显式执行 `./scripts/install-dev.ps1 -EnablePersistentDevelopmentInstall`；macOS 则先运行 `bash ./scripts/install-dev.sh --install-launcher`，再显式使用 `--enable-persistent-development-install` 注册对应的当前用户 LaunchAgent。两种持久服务均默认关闭、只以当前用户的有限权限运行。完整规则见[开发机运行与更新](docs/DEVELOPMENT.md)。

Windows 的本地安装流程会在 makensis 读取 Tauri 的 NSS-patched 主程序时创建同名不可变快照，并把它的 SHA-256、长度和随机 nonce 同时绑定到构建证明与安装后 marker；安装器返回后，脚本再核对安装器、本次证明和精确安装目标。Tauri 打包结束后会把 `target/release` 主程序恢复为未打包标记，因此它本来就不应与 NSIS 内的载荷散列相同。持久 watcher 只有拿到这个已验证 fingerprint 和成功时间后才会报告健康。升级过旧的开发启动器时须先重新运行 `.\scripts\install-dev.ps1 -NoLaunch` 刷新可信 marker。“Always Latest, Safe”只表示安全条件满足时跟随上游并安装已验证的本地快照；工作树脏、领先或分叉、网络／构建失败、或 iHub 仍在运行时都会保留现状。

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
$script = Join-Path $env:TEMP 'ihub-install.ps1'
Invoke-WebRequest https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.ps1 -OutFile $script
Unblock-File $script
& $script
~~~

~~~sh
curl -fsSL https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.sh -o /tmp/ihub-install.sh
bash /tmp/ihub-install.sh
~~~

安装脚本只下载 GitHub Release 资产，并强制要求同一 Release 提供 <code>SHA256SUMS.txt</code> 及所选安装包的有效 SHA-256 条目；清单缺失、条目不匹配或校验失败时都会安全停止。生产使用前请阅读 [发布与更新](docs/RELEASE.md)；开发机运行当前源码请使用上面的开发启动脚本。

## 插件：去中心化，但不轻率

用户可以直接输入 GitHub 仓库；官方 catalog 只是发现入口，绝不是唯一下载中心。当前 MVP 会解析 <code>owner/repo@tag</code> 或 URL <code>#ref</code>，在下载前锁定远端的实际 commit，并为 canonical <code>plugin.json</code>、整个可服务前端目录、声明图标和原生二进制写入并复核 SHA-256，绝不执行仓库脚本。同一插件 ID 首次安装后会绑定其来源与 ref，其他仓库不能借同名 ID 覆盖它；更新必须走已安装来源的只读检查与用户确认。<code>ihub.plugin.json</code> 仅作为旧包的导入兼容别名，新插件与生成模板都使用 <code>plugin.json</code>。发布者签名、界面中的逐字段权限／哈希 diff 和锁定版本回滚尚未实现，属于后续安装器能力。

也可导入公开 uTools <code>plugin.json</code> 的受限子集：<code>main</code>、<code>logo</code> 与文本型 <code>features</code> 会投影为 iHub 前端命令，并提供 <code>onPluginReady</code> / <code>onPluginEnter</code> / 带退出原因的 <code>onPluginOut</code>、宿主子输入框、可见 surface 的主窗口隐藏/显示/退出、每插件隔离且持久化的同步 <code>dbStorage</code>，以及 <code>screenColorPick</code>。存储快照会在 ready 回调前恢复，写入仍受 iHub 的数量、键长和 64 KiB 单值上限约束；取色始终经过 iHub 可见确认，仅返回一个延时采样的 HEX/RGB 值。iHub 不执行或提供 <code>preload</code>、Node、Electron、文件/图片匹配器、动态工具、系统输入或本地搜索索引；uTools 同级目录中的预加载脚本也不会作为 loopback 资源提供。iHub 的本地搜索继续完全由 Rust 原生索引实现。

~~~text
GitHub source → resolved immutable commit → manifest / integrity lock
              → immutable local store → consent → on-demand worker
~~~

插件包的 canonical 清单为 <code>plugin.json</code>（<code>ihub.plugin.json</code> 只作兼容别名），声明：

- ID、版本、iHub/API 兼容范围、TypeScript UI 入口和贡献的命令；
- 每个 OS / CPU 对应的原生 worker；
- 需要的 iHub Bridge 权限；
- 可选图标、激活事件与更新通道偏好。

完整规范、风险模型、协议与开发模板位于：

- [插件架构](docs/PLUGIN_ARCHITECTURE.md)
- [开发第一个插件](docs/PLUGIN_DEVELOPMENT.md)
- [TypeScript SDK](plugin-sdk)
- [Hello 插件](examples/ihub-plugin-hello)

### 官方插件 catalog

内置工具已经提供本地搜索、实时 9×9 放大取色、可拖拽矩形选区截图、剪贴板历史、JSON（含受限路径查询）、默认中英双向的本地离线翻译、Markdown 工作台（离线安全预览/导入/导出）、速记、进制转换、计算器、Unix/ISO/IANA 时间转换、二维码生成与图片识别、WebDAV 云盘目录浏览、录屏、批量重命名和插件创建器。截图只复用一次由用户触发的显示器或系统共享帧，确认前不写入磁盘；实时取色会话固定 9×9、限频且最长 30 秒，不注入输入，只有用户明确点“收藏”的颜色才会写入本机收藏。官方外部 catalog 还提供 OCR、联网翻译源、截图、图片工具、JSON、取色、二维码、录屏、文本工具、进制转换、批量重命名、速记、剪贴板、开发者工具、PDF、ZIP、网页动作和 iHub 启动器窗口布局；其中心 registry 在 [plugins/registry.json](plugins/registry.json)。当前 18 个条目都固定到发布 tag 的不可变 commit，并在 [plugins/registry.lock.json](plugins/registry.lock.json) 中记录 manifest、权限与全部前端／原生产物 SHA-256。

[ihub-plugin-window-manager@v1.0.2](https://github.com/neko233-com/ihub-plugin-window-manager/tree/v1.0.2) 只可对 iHub 自己的主启动器执行居中、左右贴靠和切换置顶，不能枚举、读取、聚焦或控制其他应用窗口。图片、OCR 和批量重命名的 `launcherContext` 只交接一次性元数据，仍要求用户重新选择文件／目录；文本工具与翻译只预填显式交接文本，不自动处理或发送。PDF、ZIP 和网页动作均在最小权限下提供正式 Git fallback。完整源码 checkout 中的 18 个官方项目都带固定 ID 的 `workspaceProject` 开发入口：开发安装器验证 `sourceRoot` 后优先链接当前构建产物，普通安装则全部回退到锁定 commit 的 Git 包。

官方插件会以独立 Git 仓库维护；只有发布经过审阅、锁定不可变 commit、记录 manifest/二进制 SHA-256 后，registry 才会把条目标记为可用。终端用户安装的是经验证的 Release 包或 Git 快照，而不是运行仓库内脚本。

## 搜索引擎路线

“Everything 级速度”不能靠递归扫描冒充。iHub 的进化路径是：

1. **当前 MVP**：Rust 线程池并行扫描、内存文件名/路径检索、可持久化的用户目录范围、原子 JSON 路径快照与 `path:` / `ext:` / `kind:` / `modified:` / `size:` 筛选。启动会先恢复上一次快照，再后台验证；系统监听会将已授权目录的连续变化去抖为一批路径增量替换，目录变更只重扫其子树。另有不落盘的受限正文投影，只有写出 `content:` / `body:` 才查询；它只覆盖小型 UTF-8/UTF-16 文本和源码文件，绝不等同于完整全文数据库。Windows P1a/P1c 对显式盘符根目录使用只读 Journal/MFT；P1d/P1e 会在状态目录外置、完整快照与 watcher 完整登记时把路径快照和稳定路径绑定原子保存。重启时 Journal 无变化直接复用；Journal 连续前进时仅回放有界 USN 区间、重新读取受影响路径，并在多卷回放后与落盘前双重验证。窄目录、UNC、挂载卷、未知拓扑或任何不连续都保守回退。
2. **持久化与内容**：SQLite / Tantivy 元数据与全文索引；扩展内容提取限额并交给 OCR/PDF/Office 插件。
3. **Windows 加速器（P1c/P1e）**：已接入受盘符根目录授权约束的只读 NTFS MFT 初始枚举、单次初始化窗口的 USN Journal 差分收敛，以及在状态目录位于授权卷之外时可用的跨重启零变更复用和有限增量回放。绑定保存的只是已索引稳定路径及其必需的 FRN/父 FRN 元数据；运行期普通 watcher 快照不会继承绑定。跨重启回放遇到未知事件、硬链接变更、重解析点、Journal 断档或验证竞争就自动降级，不承诺完整 Everything 数据库语义。
4. **macOS 加速器**：FSEvents 变更流、可靠的全量重扫回退，并将 Spotlight 作为可选辅助。

详细约束、数据模型和基准策略在 [搜索引擎设计](docs/SEARCH_ENGINE.md)。

## 自动更新与开机自启

- 开机自启为显式用户设置，不在首次运行时静默开启。
- 托盘始终提供显示、偏好设置、刷新索引、关于、帮助、反馈、重启和退出。帮助／反馈只能打开编译进宿主的 iHub HTTPS 地址；重启使用 Tauri 的安全进程重启 API，不拼接 shell 命令。
- 更新使用 Tauri 官方 updater，生产构建必须提供签名公钥、HTTPS endpoint 和私钥环境变量。应用侧默认只检查；设置中可由用户明确开启“自动安装已签名正式版”，它只接受版本更高且签名验证通过的 iHub Release，不拉取开发源码或插件。Windows 会移交系统安装程序、可能关闭并重新打开 iHub，macOS 在下次启动时应用；在签名 Release 发布 `latest.json` 前，它不会把本机开发包伪装为可自动更新的正式版本。插件中心会对已启用且显式选择 `stable` / `autoUpdate` 的官方锁定 Git 源做有界只读发现，仍须用户点击后才更新。候选若改变权限、网络目标或原生二进制声明会被拒绝，不能把第三方二进制的静默自动更新伪装成已完成能力。
- Windows 建议采用用户范围的 passive NSIS 安装；macOS 需要 Developer ID 签名和 notarization。
- GitHub Actions 在 Windows 和 macOS runner 上构建；发布 secrets 不会写入仓库。

## 视觉系统

界面只从 uTools / Spotlight 参考中复用信息架构、操作路径、尺寸密度与键盘可达性，不复制其配色。启动器、插件中心和内置管理面使用 Apple 高饱和功能色（蓝 `#0A84FF`、靛 `#5E5CE6`、紫 `#BF5AF2`、粉 `#FF375F`、橙 `#FF9F0A`、薄荷绿 `#30D158`、青 `#64D2FF`）与明亮半透明玻璃层级；本地搜索保留三栏参考结构，但使用饱和深蓝/靛蓝基底。启动器分组标题接入了固定版本的 React Bits BlurText（保留其许可通知），Motion 负责进入、列表和抽屉过渡；所有动画尊重 <code>prefers-reduced-motion</code>。

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
