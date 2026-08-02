# iHub 插件开发指南（v1）

iHub 插件是一个可发布的目录：前端使用 TypeScript（推荐 Vite），可选一个或多个 Windows/macOS 原生二进制后端。前端在正常路径中通过 iHub 的 iframe `postMessage` Bridge 调用宿主能力（可使用生成器自带的 bridge，也可在 SDK 发布后迁移）；原生后端通过标准输入/输出的 JSON Lines RPC 工作。

这套机制刻意支持 OCR、FFmpeg、系统自动化等原生能力。它**不是沙箱机制**：安装包含二进制的插件等同于运行本机代码。请先阅读本文的[安全与信任](#安全与信任无沙箱)章节。

## 五分钟开始

### 用 iHub 创建独立插件项目（推荐）

在桌面端安装并打开官方 **Plugin Developer Tools**（或使用内置的 **工具 → 开发者 → 创建插件项目**），先通过系统选择器选择一个已存在的父目录，再输入小写 kebab-case ID（例如 `ihub-plugin-my-feature`）。官方插件只把系统签发的短期、不透明目录授权交给宿主；页面不会提交任意路径。iHub 会在该父目录下创建同名子目录，原子预占目标路径；如果目录或同名文件已存在，操作会失败且绝不覆盖已有内容。

生成的工程默认是可立即构建、链接的 TypeScript + Vite 前端插件，包含 `plugin.json`、无外部 SDK 依赖的 `src/ihub-bridge.ts`、`worker/` 原生样例、Windows/macOS 构建脚本、`docs/JSONL_RPC.md`、`docs/ENABLE_NATIVE_WORKER.md`、`public/icon.png`、`vite.config.ts`、`package.json`、`scripts/verify-plugin.mjs` 和 README。前端 bridge 复现 iHub 的 iframe `postMessage` 契约，因此在 SDK 尚未发布到 npm 时也能直接 `pnpm install`、`pnpm dev` 和 `pnpm build`；之后可自行迁移到已发布的官方 SDK。`pnpm build` 最后只读检查 `plugin.json`、`dist/` 入口、声明图标和已显式声明的本机工件路径，不安装依赖、不启动开发服务器、不执行插件代码或原生 worker。`plugin.json` 不会预先声明不存在的二进制；需要原生能力时，开发者必须先产出并验证当前平台的 worker，再显式添加其声明。创建器不会自动执行安装、构建或其他脚本。

要在 iHub 中调试，先执行一次 `pnpm build`，就可以在 **插件中心 → 开发者 → 链接本地插件** 输入该项目的**绝对目录**。创建成功后，该开发者页会自动填入刚创建的目录；“打开项目文件夹”仍是开发者明确点击后才会调用 Finder/Explorer 的便利操作，不会运行终端命令或项目文件。要测试原生命令，再用 `scripts/build-worker.ps1`（Windows）或 `sh scripts/build-worker.sh`（macOS）构建当前平台的 Rust worker，并只把真实存在的 `bin/<target>/` 工件添加到清单；`docs/JSONL_RPC.md` 说明一行请求、一行响应的 `jsonl-rpc-v1` 约定。iHub 只保存经规范化后的路径，不复制、不改写项目文件；每次打开前端时，它只为该插件实际 `entry.frontend` 所在的已构建目录创建一条新的独立 loopback 资源租约，不会扩大 Tauri `asset:` 访问范围。每次重新构建后，关闭并重新打开插件前端即可读取新的 `dist/` 输出。这个开发链接不是文件监视器或 HMR：已打开的 iframe 不会自行刷新，且本地插件与安装快照同 ID 时会暂时优先使用本地项目；解除链接后会恢复原有安装快照。

### 从主仓示例开始

新项目优先使用桌面端的“创建插件项目”：它生成的 bridge 已随项目拷贝，不依赖未发布的 npm 包，也可以直接移动到独立 GitHub 仓库。`examples/ihub-plugin-hello` 保留为 SDK 兼容示例，适合在主仓内阅读和验证；不要把其中的 `file:../../plugin-sdk` 依赖原样带到独立仓库。

浏览器中的 `pnpm dev` 只验证页面自身，不等于在 iHub 中运行。开发期请使用上面的“链接本地插件”入口验证宿主 Bridge；它要求绝对目录、验证清单及包内相对路径，并拒绝把 iHub 托管插件目录再次链接为源码。GitHub 直装仍导入构建产物的 Git 快照，不会监听本地文件变更。

链接并打开插件后，可从 iHub 绘制的标题栏点击“分离窗口”（或在该宿主表面激活时按 `Ctrl+D`）。桌面端创建的是 800×600、可调整大小的普通 Tauri 窗口，但插件仍在原来的 loopback iframe/Bridge 边界里；它不会得到 Electron、Node、任意 shell 或 Tauri API。关闭分离窗口会释放该 iframe lease。只检查布局与安全文案时，可在浏览器打开 `http://127.0.0.1:1420/?ihubDetachedPlugin=browser.preview&ihubDetachedPreview=1`；这个专用 QA 路由不会加载你的插件、创建窗口或签发 lease，不能代替桌面端 Bridge 验证。

生成器故意**不**在 `.gitignore` 中忽略 `dist/`。GitHub 导入只读取所选 ref 已提交的文件，不会运行 `pnpm install`、`pnpm build`、CI 配置或 worker 构建脚本；可导入的发布提交必须包含实际的 `dist/` 输出、根目录 `plugin.json` 和每个清单已声明的 `bin/<target>/` 工件。

发布包的最小结构：

```text
ihub-plugin-my-feature/
├── plugin.json             # 必填，包元数据、权限与入口
├── dist/
│   └── index.html           # Vite build 输出，必填
├── assets/                  # 可选的图标、模型、静态文件
├── bin/                     # 可选的按平台原生二进制
│   ├── windows-x86_64/worker.exe
│   └── darwin-aarch64/worker
├── src/                     # 开发期前端源码
└── package.json
```

构建后保留 `plugin.json` 于包根目录；不要把它仅放进 `dist/`。
同样不要把用于分发的 `dist/` 或已声明的 `bin/` 工件忽略掉；本地 `node_modules/`、未提交的产物和 CI 临时文件不会随 Git 快照导入。

## `plugin.json`

`plugin-sdk/manifest.schema.json` 是 v1 的权威 JSON Schema。位于主仓 `examples/` 下的插件可将下面一行放进清单顶端，VS Code 等编辑器会自动校验：

```json
"$schema": "../../plugin-sdk/manifest.schema.json"
```

独立仓库在完成上面的全局链接或安装已发布 SDK 后，应改用其自身可解析的路径，例如 `"$schema": "./node_modules/@ihub/plugin-sdk/manifest.schema.json"`。

关键字段如下：

| 字段 | 必填 | 作用 |
| --- | --- | --- |
| `schemaVersion` | 是 | 固定为 `1`。 |
| `id` | 是 | 稳定的小写 kebab-case 包 ID；发布后不要改。 |
| `engines.ihub` / `engines.api` | 是 | 宿主及 API 兼容范围。 |
| `entry.frontend` | 是 | 包内前端入口，例如 `dist/index.html`；入口必须位于 `plugin.json` 所在目录的专用子目录，不能使用根目录 `index.html`。 |
| `icon` / `logo` | 否 | 包内 PNG、JPEG 或 WebP 插件身份图；优先使用 `icon`，二者不可同时声明。顶层身份图无效时插件拒绝加载；宿主只把重新编码后的 PNG data URL 交给界面。 |
| `contributes` | 否 | 命令、搜索提供器、设置和快捷动作的静态声明。 |
| `activationEvents` | 否 | `onStartup`、`onSearch`、`onCommand:<id>` 或 `onFile:<ext>`。 |
| `permissions` | 是 | 前端 Bridge 的能力请求；空对象也必须明确写出。完整安装确认将在生产分发流程中加入。 |
| `backend` | 否 | 与前端配套的原生二进制及其平台目标。 |
| `update` | 否 | 声明稳定/测试通道和自动检查偏好。插件中心打开时、以及保持打开期间每 30 分钟，会对已启用、带完整 SHA-256 运行文件记录的 immutable source lock、`stable` 且来自官方 `neko233-com` HTTPS GitHub 命名空间的 `autoUpdate: true` 插件做有界只读检查；它不是全局后台任务，且绝不会后台下载、替换或启动候选代码。 |

`update.autoUpdate: true` 的名称不代表静默升级授权：它只允许上面的官方来源**发现**新 commit。用户仍要在插件中心选择“应用更新”并确认；宿主会再次确认 ref 仍指向已审阅 commit，并拒绝任何权限、原生二进制或命令执行声明变化。要接受这类信任面变化，必须卸载受管快照并通过 GitHub 导入重新完成信任确认。旧版 lock 若没有运行文件 SHA-256，不会参与自动探测；它仍可由用户手动“检查更新”，重新导入后才会升级为可自动探测的受验证快照。

一个不依赖原生二进制的最小清单：

```json
{
  "$schema": "../../plugin-sdk/manifest.schema.json",
  "schemaVersion": 1,
  "id": "ihub-plugin-my-feature",
  "name": "My feature",
  "version": "0.1.0",
  "engines": { "ihub": ">=0.1.0", "api": "^1.0.0" },
  "entry": { "frontend": "dist/index.html" },
  "activationEvents": ["onCommand:open-my-feature"],
  "contributes": {
    "commands": [{ "id": "open-my-feature", "title": "Open My feature" }],
    "searchProviders": [
      { "id": "my-search", "title": "My feature", "trigger": "my ", "priority": 10 }
    ]
  },
  "permissions": {
    "notifications": true
  }
}
```

清单中的包内路径不能是绝对路径，也不能借由 `..` 或符号链接离开插件根目录。图像路径还不能包含控制字符、冒号、空组件、`.` 组件、Windows 设备名或以点/空格结尾的组件。`icon`、`logo` 以及静态 `contributes.commands[].icon` 只应使用经过 Rust 图像解码器验证的有界 PNG、JPEG 或 WebP；SVG、脚本、损坏图片、过大文件和过大像素尺寸都不可发布。顶层 `icon`/`logo` 无效会拒绝插件；为了兼容旧包，不可用的命令级图标会被安全忽略并回退到宿主默认图标，不会因此拒绝整个插件。界面永远不会收到本地路径或原始文件。每份清单最多 64 个静态命令、最多引用 32 个不同图像候选；运行时 `commands.register` 不能添加图标，SDK 还会防御性剥离无类型 JavaScript 传入的 `icon`。`entry.frontend` 必须放在专用构建目录（通常是 `dist/`）内：iHub 只会把**该入口所在目录**作为 loopback 静态资源根，绝不会把包含 `plugin.json`、`.env`、`bin/` 或源码的包根暴露给浏览器。每个 `backend.binaries[].target` 只能声明一次：`windows-x86_64`、`windows-aarch64`、`darwin-x86_64`、`darwin-aarch64`。

每个 `contributes.commands[]` 的 `id` 必须唯一，并可选 `execution: "frontend" | "native"`。不写时保持兼容行为：含原生 worker 的插件默认启动 native 命令，其余插件默认打开前端。带 worker 的插件若要先显示自己的 UI（例如 OCR 先让用户选图），应在该入口命令明确写 `"execution": "frontend"`；原生命令只能通过清单锁定的 worker 执行。原生命令还可显式写 `"run": { "timeoutMs": 900000 }`：该字段只能与 `execution: "native"` 一起使用，范围为 1,000–1,800,000 ms；省略时保持兼容的 60 秒上限。

### 清单全局快捷键

全局快捷键完全由 Rust 驻留宿主注册，运行中的 iframe 不能申请、修改或移除系统快捷键。插件必须先声明 `"permissions": { "globalShortcut": true }`，再使用命令上的 `shortcut` 简写，或 `contributes.globalShortcuts[]` 把一个快捷键映射到已声明命令／启动器关键词（二选一）：

```json
{
  "contributes": {
    "commands": [{
      "id": "open-my-feature",
      "title": "Open My feature",
      "keywords": ["my feature"],
      "shortcut": "Alt+KeyM"
    }],
    "globalShortcuts": [{
      "id": "search-my-files",
      "shortcut": "CmdOrCtrl+Alt+KeyM",
      "keyword": "my files"
    }, {
      "id": "open-feature-explicitly",
      "shortcut": "Alt+Shift+KeyM",
      "commandId": "open-my-feature"
    }]
  },
  "permissions": { "globalShortcut": true }
}
```

宿主只接受 `CmdOrCtrl`、`Alt`、`Shift` 加受限物理键名的跨平台语法；至少包含 `CmdOrCtrl` 或 `Alt`。`Alt+F4`、`Alt+Space`、恢复用 `Alt+Shift+Space`、当前启动器快捷键、同一插件内重复项以及跨插件重复项都不会注册。每个插件的命令简写和插件级映射合计最多 16 个，整个宿主最多激活 128 个。跨插件重复时全部失败关闭，不按安装顺序抢占；操作系统占用也只把该项标记为失败，不会移除或影响主 `Alt+Space`。插件中心会显示每项 `registered`、`blocked`、`unavailable` 或 `inactive` 状态及失败原因。

命令映射仍走普通可见激活流程：前端命令打开受约束 iframe，原生命令保留首次执行的二进制信任确认。关键词映射只打开启动器并填入有界关键词，不直接执行模糊结果。安装后的常规 Git 更新不能静默更换快捷键目标；这类信任声明变化必须重新导入审阅。

## 前端 SDK

SDK 不序列化 JavaScript 回调给 Rust；它在插件 iframe 中保留回调，并由宿主桥把一次命令或搜索请求送回该 iframe。最小用法：

```ts
import { bootstrapPlugin } from "@ihub/plugin-sdk";

await bootstrapPlugin("ihub-plugin-my-feature", async (ihub) => {
  await ihub.commands.register(
    { id: "open-my-feature", title: "Open My feature" },
    async () => ({ message: "Opened", close: true }),
  );

  await ihub.search.register(
    { id: "my-search", title: "My feature" },
    (request) => [{
      id: "result-1",
      title: `Search: ${request.query}`,
      score: 1,
      payload: { query: request.query }
    }],
  );
});
```

`search.register()` 的 `id` 必须与 `contributes.searchProviders` 中的同名声明匹配；宿主会拒绝未声明的运行时提供器。用户在启动器中输入匹配文本时，iHub 才惰性载入该插件的单个隐藏 iframe；在 `lifecycle.ready` 之后，最多同时查询 3 个匹配提供器，每个请求在原生侧按 `requestId` 关联，并在 280ms 后超时。单个提供器最多返回 6 项，启动器最多展示其中 3 项；标题、载荷和结果数量均有上限。未注册、加载失败、超时或返回无效 JSON 的提供器只会缺席本次结果，不会阻塞本机搜索。

带 `trigger` 的提供器仅在输入以该前缀开头时被调用，传给处理器的 `request.query` 已移除前缀；无 `trigger` 的提供器可参与普通文本搜索。选择一项插件搜索结果会打开该插件前端，并向 `ihub://plugin/<id>/event/search.select` 发送 `{ requestId, providerId, resultId, payload }`。可用 `ihub.events.on("search.select", listener)` 消费它；`actions` 目前仅是结果元数据，尚没有独立的启动器动作执行 API。

`PluginContext` 提供的宿主 API：

| API | 对应权限 | 说明 |
| --- | --- | --- |
| `commands.register` | 无 | 注册命令处理器。 |
| `search.register` | 无 | 注册低延迟搜索提供器。 |
| `subInput.set/setValue/remove/focus/blur/select` | 无（仅可见 surface 租约） | 控制由可信 iHub 宿主绘制的有界文本输入。回调留在 iframe 内；隐藏搜索 runtime、原生 worker 和失效租约不能创建、修改或聚焦输入。关闭、替换、禁用或 dispose 插件页会清除值和回调。 |
| `settings.get/set` | 无 | 插件命名空间下的设置存储。密钥设置必须在清单上标注 `secret`。 |
| `clipboard.readText/writeText` | `clipboard.read/write` | 读写剪贴板文本；写入最多 48 KiB UTF-8，并经过宿主串行重试通道。 |
| `clipboard.history.snapshot` | `clipboard.history` | 仅在插件主动调用时返回最多 36 条、已经由用户启用的内置纯文本历史；不会开启采集、读取当前系统剪贴板或修改全局历史。 |
| `screenCapture.acquireFocusLease/releaseFocusLease` | `screenCapture: true` | 仅在可见 `Surface` 的活动原生租约上，可信 host 才向跨 origin iframe 委派 `display-capture`；隐藏搜索 runtime 和未声明插件没有委派。焦点租约只在浏览器 `getDisplayMedia` 系统选择器显示期间阻止启动器因失焦隐藏，不授予屏幕像素、录制、全局快捷键或原生捕获 API。 |
| 浏览器 `getUserMedia({ audio: true })` | `microphone: true` | 仅向 Rust 验证的可见 `Surface` 活动租约委派 `microphone`；它与 `screenCapture` 独立，不授予后台、隐藏 runtime 或原生 worker 录音能力，也不会跳过浏览器／OS 同意。 |
| `cursorColor.sampleOnce` | `cursorColor: true` | 仅当前可见插件页可请求。iHub 宿主必须先由用户确认，再固定等待 2 秒读取光标下一个像素；只返回 `hex`/`rgb`，不返回坐标、截图、显示器/窗口信息，也不支持后台轮询。 |
| uTools `screenCapture(callback)` | 已验证 uTools 兼容包；原生 iHub 插件使用 `screenCapture: true` | 可信父层先显示确认，隐藏当前 iHub 窗口后只截取主显示器一帧，再由用户在宿主选区界面裁剪。完整画面不发送给 iframe，回调只收到不超过 16 MiB 的选区 PNG Data URL；隐藏 runtime、过期租约和后台调用均拒绝。 |
| `launcherContext.consume` | `launcherContext.text/files/image` | 仅在用户明确选择一次已声明的前端插件命令后，按该次动作签发的不透明 `contextId` 可消费一次。文本有上限；文件只有 canonical 名称/类型/大小与无路径 handle；图片只有 PNG 元数据与无像素 handle。不会读取剪贴板、解析路径或变成文件授权。 |
| `windowManagement.manageLauncher` | `windowManagement: true` | 仅对 iHub 标签为 `main` 的主启动器执行固定动作：居中、贴靠左侧、贴靠右侧或切换置顶。不会读取、枚举、聚焦或控制其他应用窗口，也不接受任意坐标。 |
| `notifications.show` | `notifications` | 通过原生系统通道显示本机通知；宿主固定标出插件 ID 来源，并限制为每插件每 10 秒最多 5 条。 |
| `shell.openExternal/openPath` | `shell.*` | 打开 URL 或文件路径。 |
| `filesystem.selectDirectory` | `filesystem.read: ["user-selected"]` | 打开原生文件夹选择器，返回当前插件专属、15 分钟过期的 `grantId`。 |
| `filesystem.selectFiles` | `filesystem.read: ["user-selected"]` | 打开原生多文件选择器；网页只获得文件名/大小与短期 `grantId`，不会获得本地路径。 |
| `filesystem.previewBatchRename` | `filesystem.read: ["user-selected"]` | 只在该 `grantId` 对应的目录生成安全重命名预览。 |
| `filesystem.applyBatchRename` | `filesystem.write: ["user-selected"]` | 只接受同一 `grantId` 和宿主刚生成、5 分钟内且单次有效的 `previewId`。 |
| `native.runCommand` | `nativeApi` | 运行清单中声明的当前平台 worker；若传入文件授权，路径只放入 worker 的 JSON 输入，网页不可见。默认最多 60 秒，原生命令可通过受审阅的 `run.timeoutMs` 将前台等待上限提高至最多 30 分钟。 |
| `events.on` | 取决于事件 | 订阅宿主发送给当前插件的事件。 |
| `logger` | 无 | 写入带插件 ID 的宿主日志。 |

`bootstrapPlugin` 运行期间还会安装一个冻结的最小兼容对象，并让 `window.utools === window.rubick`。兼容对象提供 `setSubInput`、`removeSubInput`、`setSubInputValue`，以及映射到上表既有权限检查的 `copyText`、`showNotification`、`shellOpenExternal`、`shellOpenPath`、`screenColorPick`；另有只读的主题、平台和 `"main"` 窗口类型查询。同步 `boolean` 返回值只表示参数已通过 SDK 本地校验且异步宿主请求已排队，真正的权限或租约拒绝仍写入 `BootstrapOptions.onError`。兼容层不会覆盖页面已有的同名全局，并在 runtime dispose 时恢复原值。

直接导入的公开 uTools 包使用另一套宿主固定注入层：除 ready/enter/out、分离窗口就绪后的单次 `onPluginDetach`、`dbStorage` 和逐次确认取色外，还提供原生 `copyText(value)`、`showNotification(body, clickFeatureCode?)`、Windows 系统 `shellBeep()`、受限 `shellOpenExternal(url)`、逐次确认的 `shellOpenPath/shellShowItemInFolder/shellTrashItem`、同步系统 `getFileIcon`、页面内 `findInPage/stopFindInPage`、官方签名一致的 `setSubInput`、`removeSubInput`、`setSubInputValue`、`subInputFocus/Blur/Select`，以及仅限当前可见活动 surface 的 `hideMainWindow`、`showMainWindow`、`outPlugin` 和 `setExpendHeight`。`shellBeep` 与通知共用每插件每 10 秒 5 次的宿主限流，不能成为后台持续发声通道。Windows 上的 `hideMainWindowPasteText`、`hideMainWindowPasteImage`、`hideMainWindowPasteFile` 与 `hideMainWindowTypeString` 也要求同一可见活动租约；宿主接受请求后隐藏 iHub，等待前一窗口恢复焦点，再发送 Ctrl+V 或分批 Unicode 输入。图片沿用 `copyImage` 的 PNG 与解码内存上限；文件沿用 `copyFile` 的逐次确认、最多 16 项、8 KiB 路径与对象身份防护，确认前不探测文件系统。粘贴文本沿用 48 KiB 上限，直接键入最多 4,096 个字符且不接受 NUL；未经过 Windows 运行验收的平台明确拒绝，不模拟成功。页面查找限制为 512 个字符和官方布尔选项，不把搜索文本发给宿主。`getFeatures/setFeature/removeFeature` 也会同步维护页面缓存并持久化到插件自己的宿主命名空间；每插件最多 64 个动态功能，每项最多 16 个直接文本指令，启动器会立即刷新并在点击时把原始 feature `code` 交回插件。运行时图标只作为兼容数据返回，不成为未经验证的宿主 artwork；文件、图片、正则和窗口 matcher 不会升级成本地索引或路径权限。复制文本最多 48 KiB UTF-8，经过活动租约和 Rust 串行重试通道写入系统剪贴板，不依赖 iframe 的浏览器剪贴板权限。外链最长 2048 字符，只允许无控制字符且 host 非空的绝对 `http/https` URL 或非空 `mailto` 收件人；不接受文件路径、其他协议或命令字符串。分离回调由可信 host 在新窗口 Bridge ready 后触发，隐藏 runtime 不会收到；回调晚于窗口类型事件注册时会立即补发一次。兼容通知正文最多 1000 字符，并与 SDK 通知共用每插件每 10 秒 5 条的原生限流；Windows 上的可选 `clickFeatureCode` 必须指向插件当前仍声明的静态或动态功能，通知点击时宿主会再次校验并通过可信启动器事件激活该功能，其他平台明确拒绝而不伪造点击。`readCurrentFolderPath/readCurrentBrowserUrl` 只在可见活动 surface 中可用：iHub 在本轮启动器显示前只保存一个外部前台 HWND/PID，每次读取都弹出确认并重新校验同一窗口，不枚举其他窗口、不模拟输入且不读取剪贴板。文件夹读取仅接受 Windows Explorer 的本地文件夹视图并已完成 Windows 实机链路验证；网址读取仅接受受支持浏览器通过 Windows UI Automation 暴露的非密码地址编辑框，其他平台明确拒绝。高度请求只接受 100–900 的整数内容像素；iHub 可信标题栏另占 60 像素，切换插件会恢复默认内容高度。窗口请求先经过原生侧“已验证 uTools 包 + 活动租约”复核，响应成功后才由可信 React 父层隐藏/显示 iHub、调整主窗口高度或退出当前插件。`getAppName/getAppVersion` 返回 iHub 的真实产品信息，未接入 uTools 账号体系时 `getUser()` 明确返回 `null`。

`screenCapture(callback)` 使用官方的无参数、异步回调签名。调用先停在可信父层；用户点击“开始截图”后，原生宿主在活动 surface 租约与插件权限仍有效时保留一次 native-operation reservation，隐藏当前 iHub 窗口并截取主显示器。恢复窗口后，完整 PNG 只进入父层内存选区编辑器；取消不会调用回调，完成时只返回用户裁剪的 PNG Data URL。选区至少 2 × 2、单边最多 8192 px、总计最多 2400 万像素且编码最多 16 MiB。插件不能传显示器、坐标、矩形或延迟参数，隐藏搜索 runtime 不能调用，关闭/reload/更新/停用/卸载会使等待中的请求失效。

`showOpenDialog(options)` 与 `showSaveDialog(options)` 保持 uTools 的同步返回签名。请求只可从当前可见 surface 的随机 loopback origin 发起，并带宿主私有请求头、严格 JSON 长度与字段校验；资源更新在弹窗期间被 native-operation reservation 阻止。可信 dispatcher 将弹窗调度到 Tauri UI 线程并绑定 iHub 主窗口，取消分别返回 `undefined`，成功只返回用户在系统原生选择器中明确选中的本地路径。Windows 当前支持文件/文件夹、单选/多选、默认路径、最多 16 组扩展名过滤器，以及保存对话框的系统覆盖确认；未完成实机验证的 macOS security-scoped bookmark、标签字段、隐藏文件、alias 和 recent-list 等 Electron 特殊选项会明确拒绝，不静默降级。

`redirect(label, payload?)` 支持官方的单一指令名和 `[插件名, 指令名]` 两种定位方式，以及 text、PNG image、files 三种有界交接。Rust 只从当前启用并再次验证为 uTools 导入包的命令中做不区分大小写的精确匹配；未安装目标明确失败，不会伪装成应用市场跳转。唯一目标直接切换，多个同名目标回到可信启动器供用户选择。接收方仅从 host-owned command event 得到规范化的 `{ code, type, payload, from: "redirect" }`；隐藏 surface 不能发起，非候选选择、隐藏窗口或新一轮启动器会立即丢弃暂存内容。

`onMainPush(callback, onSelect)` 通过宿主固定的 `utools-main-push` 搜索提供器接入主启动器。只有静态或动态 feature 明确设置 `mainPush: true` 时才投影提供器；隐藏 iframe 在查询时仅接收最多 512 bytes 的当前文本，并只对匹配的直接文本指令同步调用 `callback({ code, type: "text", payload })`。每次最多接受 6 个可 JSON 序列化且小于 6 KiB 的 option，`text/title` 均限制为 320 个字符，包内 `icon` 不会绕过宿主 artwork 边界。选择时 Rust 只从 60 秒内的原生已签发搜索快照取回 action/option，再以一次性交互 ID 调用 `onSelect`；只有严格返回 `true` 才打开插件并触发 `onPluginEnter`，`false/undefined` 保持静默。官方示例中的同步粘贴动作可在这一短暂交互区间使用；令牌绑定精确 iframe 租约，并在完成或最长 5 分钟（为文件确认弹窗保留时间）后失效。正则、任意文本、图片、文件与窗口 matcher 尚未投影到该兼容搜索阶段，不能被伪装为已经支持。

`onDbPull(callback)` 会保存回调并只接受未来宿主云同步通道送达的文档数组。当前 iHub 没有跨设备 uTools 云同步，因此不会伪造 pull 事件；与此一致，`replicateStateFromCloud()` 仍如实返回 `null`。

Windows 上还实现了官方同步显示器族：`getPrimaryDisplay/getAllDisplays/getCursorScreenPoint/getDisplayNearestPoint/getDisplayMatching` 与 `screenToDipPoint/dipToScreenPoint/screenToDipRect/dipToScreenRect`。它们通过当前随机 loopback 租约的只读同步端点取得实时显示器和光标快照，不使用异步 Promise 冒充同步返回。显示器包含 Electron `Display` 的标准形状、DIP bounds/work area、原生像素 origin、有效 DPI scale 与稳定的设备名派生 ID；坐标转换始终选择包含或最近的显示器，并按该显示器缩放相对坐标。端点只接受同源、无请求体的 GET，最多投影 32 个活动显示器，不返回窗口句柄、窗口清单或屏幕像素；未在真机验证的平台明确抛错。

`desktopCaptureSources(options)` 通过 Chromium/WebView2 的系统屏幕选择器兼容旧式录屏调用，而不是静默枚举所有窗口。它验证官方的 `types`、`thumbnailSize`（最大 512 × 512）与 `fetchWindowIcons` 形状，在同一用户手势内启动 `getDisplayMedia({ video: true, audio: true })`，并用活动 focus lease 防止系统选择器夺焦时隐藏启动器。用户只会得到自己选中的一个 source；其 ID 在当前页面内一次有效、60 秒过期，带有内存 PNG thumbnail 和 `NativeImage` 常用只读方法。随后以该 ID 调用旧式 `getUserMedia({ video: { mandatory: { chromeMediaSource: "desktop", chromeMediaSourceId }}})` 会消费已经授权的同一条 `MediaStream`，不会再次枚举或切换来源；未消费、页面退出、重新选择或超时都会停止 tracks。隐藏 runtime 不获得 `display-capture` Permissions Policy，取消系统选择器会按浏览器错误拒绝 Promise。该兼容层不会伪造所有窗口列表、窗口句柄、应用图标或未被用户选择的缩略图。

`copyImage(value)` 当前接受 PNG Data URL 或 PNG `Uint8Array`，同步 `true` 仍只表示本地校验通过并已排队。压缩数据最多 4 MiB，宿主在写入剪贴板前重新验证 PNG 签名、8192 px 单边、1200 万像素和 48 MiB RGBA 上限；每个插件 frame 同时只允许一个大图片请求。字符串文件路径暂不接受，后续只能通过系统选择器签发的路径授权接入，不能直接开放任意本机路径。

`copyFile(value)` 接受一个路径或最多 16 个路径，且只允许当前可见活动 surface 调用。确认前只检查路径字符串的绝对形式、去重、控制字符和 8 KiB 总上限，不访问文件系统，因此拒绝确认不会成为文件存在性探针；原生警告框会逐项展示插件提交的原始目标。用户明确允许后，宿主才解析对象，并拒绝网络/设备命名空间、符号链接、缺失项和非普通文件/文件夹；通过校验的对象会保持身份防护直到写入系统剪贴板。该确认与通知、系统提示音共用每插件每 10 秒 5 次的可见提醒限流，避免插件连续弹窗。

`getPath(name)` 在插件页面任何脚本执行前同步提供官方列出的 `home/appData/userData/temp/exe/desktop/documents/downloads/music/pictures/videos/logs` 投影；得到路径字符串本身不会授予读取、写入或启动该路径的能力。`getNativeId()` 返回宿主为每个插件分别生成并持久化的随机标识，同一插件重启后稳定，但不同插件无法用它关联同一台设备，也不读取硬件序列号。

`shellOpenPath/shellShowItemInFolder/shellTrashItem` 保持官方同步 `void` 签名，但请求只允许当前可见活动 surface 发起。宿主在任何文件系统探测前先展示插件 ID、原始绝对路径和动作说明；用户允许后才接受本地普通文件或文件夹，拒绝 UNC、设备命名空间、符号链接／重解析点和缺失对象。打开与定位会保留对象身份防护直到系统调用返回；移到回收站绝不回退为永久删除，Windows Shell 的路径型回收调用需要在最后一次对象复核后释放阻止删除的句柄。三类请求与通知共用提醒限流，未经过运行验证的平台明确报错。

`getFileIcon(filePath)` 同步返回 Windows 原生 PNG Data URL。`.txt` 这类最多 16 个 ASCII 字母数字的扩展名和特殊值 `folder` 使用 `SHGFI_USEFILEATTRIBUTES`，不会探测同名文件；真实绝对路径则沿用本地对象校验与身份绑定，拒绝网络、设备、重解析点和缺失项。它只通过当前随机租约路径、自定义同源请求头、固定 `{ path }` JSON 和 12 KiB 请求上限访问宿主；原生 Shell worker 的等待上限为 650 ms，超时或无法取得图标返回空字符串。浏览器预览和宿主未返回图标时仍使用中性占位，不以自制 EXE 图标冒充系统图标。

`utools.db` 及其 `promises` 成员均提供 `get(id)`、`put(doc)`、`remove(idOrDoc)`、`bulkDocs(docs)`、`allDocs(prefixOrIds)`、`postAttachment(id, bytes, mime)`、`getAttachment(id)` 与 `getAttachmentType(id)`。同步版通过当前插件随机 loopback origin 内的固定协议直接取得真实宿主结果；只接受随机租约路径、自定义同源请求头、固定 JSON 形状和 15 MiB 请求上限，不使用“页面缓存后异步落盘”模拟同步成功。文档按已验证的插件 ID 分库持久化，写入使用 `_rev` 乐观并发控制和原子文件替换；单文档（包含宿主写入的 `_id/_rev`）最多 1 MiB，每库最多 2,048 个文档、32 MiB，单次 bulk 最多 16 个文档且输入最多 8 MiB。`allDocs()` 按 `_id` 排序，无参数返回全部，字符串选择器匹配前缀，字符串数组按请求顺序去重取回。附件接受 1 byte–10 MiB 的 `Uint8Array`，只允许在新 ID 上创建，使用独立原子文件和 SHA-256 元数据校验，随对应文档删除；插件卸载时会清除该插件的数据库与附件。iHub 不提供 uTools 云同步，因此同步及 Promise 版 `replicateStateFromCloud()` 均如实返回 `null`。

`utools.getCopyedFiles()` 保留官方同步返回形状（包括兼容字段名 `isDiractory`）。它只对当前有效的可见插件 surface 开放；隐藏搜索 runtime 会收到拒绝。Windows 在读取前验证原生 `CF_HDROP` 源不超过 256 KiB，并在读取前后校验剪贴板序列号，随后只返回最多 32 个仍可解析、非重解析点的本地文件或文件夹；不读取文件内容，也不把路径写入历史或日志。剪贴板没有文件、正忙、在校验期间变化或无法证明边界时返回空数组。

Windows 上的 `simulateKeyboardTap`、`simulateMouseMove`、`simulateMouseClick`、`simulateMouseDoubleClick` 与 `simulateMouseRightClick` 使用真实 `SendInput/SetCursorPos`，不返回模拟成功。它们只允许当前可见插件 surface 调用，每次执行前由宿主显示插件来源、按键组合或物理屏幕坐标并要求用户确认，与通知共用每插件每 10 秒最多 5 次的限流；隐藏 runtime、屏幕外坐标、非整数坐标、未知按键或修饰键会被拒绝。省略单击坐标时固定使用确认对话框出现前捕获的当前指针位置，避免确认按钮改变操作目标；部分键盘注入失败时宿主会补发按键释放事件，降低修饰键滞留风险。当前仅 Windows 10/11 x64 完成实现，其他平台明确报错。

这不是 Electron/uTools preload 的复刻。iHub 明确不提供 `require`、`fs`、`child_process`、`remote`、任意 preload/BrowserWindow/命令行、未授权本机路径、未经逐次确认的键鼠模拟或其他应用窗口枚举；上述有界显示器元数据也不包含窗口、句柄或像素。依赖这些 API 的旧插件必须迁移到 iHub 的声明式权限、用户选择授权或清单锁定 native worker，不能通过兼容对象绕过。

普通设置会以原子方式写入 iHub app-data 中、按插件 ID 隔离的 JSON 存储，并在重启后保留。`contributes.settings` 中声明 `secret: true` 的键绝不会写入该 JSON：它们只保存在当前 iHub 进程的内存中，重启、禁用、卸载或切换插件源后必须重新输入。这样不会把 API key 等凭据悄悄落盘；需要跨重启保留的凭据应暂时由插件自己的受控原生 worker 管理，直到 iHub 接入系统凭据库。`settings.set()` 的桥接响应包含 `{ saved: true, persistent: boolean }`，其中 secret 键的 `persistent` 为 `false`。

不要主动导入 `@tauri-apps/api`。iHub 会为 `entry.frontend` 创建每 iframe 独立的 `127.0.0.1` loopback 资源来源；SDK 在该 iframe 中通过父窗口 `postMessage` Bridge 访问宿主。该 remote origin 不匹配 iHub 的 Tauri capability，父窗口还会验证消息来源窗口与精确 origin，并由宿主附加租约。`window.__IHUB_PLUGIN_API__` 是 SDK 预留给未来受控宿主表面的接口，当前 launcher 不会注入它。SDK 不提供直接调用 Tauri 的后备路径；这条边界只覆盖 TypeScript 前端，不会限制插件附带的原生 worker，因此 GitHub 导入的前端和二进制仍必须仅来自你信任的发布者。浏览器预览或测试必须显式传入 `createDevelopmentBridge()` 或自定义 `HostBridge`。

Vite 插件必须保留 `base: "./"`（生成模板已配置）。iHub 的前端 URL 含有短期 `/v1/<token>/` 前缀；默认 `base: "/"` 会让产物请求错误的根路径 `/assets/...`，从而无法加载静态资源。

### 启动器上下文交接（显式、一次性）

`launcherContext` 是给 “用此文本翻译”“用此图片 OCR” 一类动作准备的**宿主/SDK 原语**，不是剪贴板、文件系统或图片读取权限。插件只需在自己的 `plugin.json` 写出真正需要的最小集合：

```json
{
  "permissions": {
    "launcherContext": {
      "text": true,
      "files": true,
      "image": true
    }
  }
}
```

三个 flag 独立审核：`text` 不会带来文件元数据，`files` 不会带来路径或读取权，`image` 不会带来 PNG 字节、屏幕像素、录屏流或 `getDisplayMedia`。文件输入会先在宿主规范化并确认仍是普通文件/文件夹；交给 iframe 的只有 `name`、`kind`、可选 `size` 和随机 `handleId`。该 handle 目前没有“解析为路径/字节”的 Bridge API。图片同样只有受限 `image/png` 元数据和 handle。原生 worker 的既有能力不因此变强：不能把这个上下文 ID 当作 `fileGrantId`，也不会自动把它传给 worker。

插件前端只消费、从不签发上下文。一个可信 iHub 父界面必须严格按下面顺序实现动作；调用 API 本身不能证明用户意图，因此**不得**从搜索建议渲染、计时器、启动事件、剪贴板监听或后台 runtime 调用第 2 步：

1. 用户点击一个可见的“用 <插件> 处理当前内容”动作；父界面确认目标是同一插件已经声明的 `execution: "frontend"` 命令，并从当前这次显式粘贴/选择得到源内容。
2. 父界面只能在该 iframe 的 `lifecycle.ready`、命令注册和原生命令事件订阅都已完成后，调用受 Tauri 主界面限制的 `issue_plugin_launcher_context({ pluginId, commandId, context, frontendLeaseId })`。它会校验大小、路径/文件状态、图片元数据、每个 `launcherContext.*` flag 及这个**当前**前端租约，并返回 60 秒过期的 `contextId`；它不会主动打开 iframe 或推送内容。
3. 父界面立即调用 `invoke_plugin_frontend_command({ pluginId, commandId, launcherContextId: contextId, frontendLeaseId })`。宿主会再次校验同一租约，只把 `{ contextId, expiresInMs }` 放入这一次命令事件，拒绝跨插件、跨命令、跨租约或重复分派。
4. 命令处理器自行决定是否使用，并在处理器内显式消费一次：

```ts
await ihub.commands.register(
  { id: "translate-selection", title: "Translate selection" },
  async (invocation) => {
    const transfer = invocation.launcherContext;
    if (!transfer) return { message: "No selection was handed to this action." };

    const selected = await ihub.launcherContext.consume(transfer.contextId);
    // selected.text / selected.files / selected.image are bounded metadata.
    return { message: selected.text ? "Selection received" : "No text selected" };
  },
);
```

消费前关闭/reload iframe、停用、更新、解除本地链接或卸载插件都会撤销 token；重复消费、不同插件、不同命令、不同租约或过期 token 都会失败。当前主启动器的上下文建议会进入专用的“选择插件命令”面板：它只列出**已启用、已安装、带 frontend 入口**，且 `launcherContext.text/files/image` 与当前类别精确匹配的命令；浏览或搜索候选不会签发 token。用户先选择命令，再点击“确认并运行”后，主界面会等待该 iframe 完成 `lifecycle.ready`、命令注册和事件订阅，然后才按第 2–3 步签发并分派。主界面为这次确认保留独立 generation：任何隐藏、关闭、焦点重开、Escape、来源更新或租约替换都会先作废 generation，再丢弃内存源；它会持续保存已分派 token 的精确 generation/租约撤销句柄，直到插件消费、宿主 TTL 或表面释放，因此已签发**或已分派但未消费**的 token 都会立即撤销。该界面不读取 ambient clipboard，也不会把 `contextId` 写入设置、日志、历史或网络请求。

### 用户选择目录与批量重命名

文件系统权限不是全盘路径通行证。插件应由用户点击 UI 后调用 `ihub.filesystem.selectDirectory()`，并显式处理 `{ cancelled: true }`。用户选中的目录会被宿主规范化，返回的 `grantId` 仅属于发起调用的插件、不会持久化，并在 15 分钟后过期。

批量重命名应使用下面的两阶段流程：

```ts
const selected = await ihub.filesystem.selectDirectory();
if (selected.cancelled) return;

const preview = await ihub.filesystem.previewBatchRename({
  grantId: selected.grantId,
  find: "draft-",
  replace: "final-",
  useRegex: false,
});
// 先在插件 UI 展示 preview.items 和 preview.errors，再让用户确认。
if (preview.canApply && preview.previewId) {
  await ihub.filesystem.applyBatchRename({
    grantId: selected.grantId,
    previewId: preview.previewId,
  });
}
```

宿主只枚举所选目录的直接普通文件，并在预览与执行时分别复查路径、符号链接、重名冲突和过期记录。前端不能传 `directory` 或 `items` 给执行调用；预览令牌与授权目录、插件 ID 绑定且用后即失效。不要试图把 `grantId` 保存到设置、日志或远端服务。

### uTools 加密键值存储

`dbCryptoStorage.setItem/getItem/removeItem` 保持官方同步签名，并与 `dbStorage` 一样在 `onPluginReady` 前完成页面缓存水合。每个插件的随机 256 位密钥只保存在 Windows Credential Manager 或 macOS Keychain；app-data 文件除版本和插件命名空间外只保存 AES-256-GCM 密文与随机 nonce，AEAD 附加数据与加密信封同时绑定插件 ID，交换、篡改或丢失凭据时会拒绝读取和覆盖。键和值都不会明文落盘；每插件最多 128 键，键最多 48 UTF-8 字节，单值最多 64 KiB，总明文最多 512 KiB。普通更新和本地开发重载会保留密文，受管插件卸载会先删除密文再清理系统凭据。

### uTools 原生文件拖拽

`startDrag(path | paths)` 保留官方的 `void` 签名，并在 Windows 上进入 Shell/OLE 原生文件拖放循环。它只接受当前可见 uTools surface 在本轮 `showOpenDialog` 中明确选中的文件或文件夹，最多 16 项；宿主把返回路径与插件 ID、随机 lease ID、对象类型和文件系统身份绑定，拖动前重新打开并核对同一对象，未授权路径不会触发文件系统探测。文件被替换、surface 重载/关闭、插件停用/更新/卸载都会撤销授权；拖放进行时保留对象句柄和 native-operation reservation，因此路径不能在 Shell 接管前被同名对象替换，也不能与插件源变更并发。macOS 尚未完成真机拖放验收时会明确拒绝，不伪造成功。

### uTools 账号与付费边界

`getUser()` 在 iHub 没有 uTools 登录会话时返回官方定义的 `null`，`isPurchasedUser()` 返回 `false`；临时 token 和支付记录 Promise 会明确拒绝，购买/支付入口不会回调成功。iHub 不生成虚假 uTools 身份、令牌、订单或授权状态。本地开发链接中的 uTools 插件由 `isDev()` 返回 `true`，受管安装快照返回 `false`。

### 用户选择文件与原生 worker

图片、OCR 等插件不应要求用户把本地绝对路径粘贴进网页。声明 `filesystem.read: ["user-selected"]` 与 `nativeApi: true` 后，前端可以在用户点击时选择文件，再将短期 `grantId` 交给自身已声明的 worker：

```ts
const selection = await ihub.filesystem.selectFiles();
if (selection.cancelled) return;

const result = await ihub.native.runCommand({
  commandId: "recognize-image",
  fileGrantId: selection.grantId,
  input: { language: "eng" },
});
```

网页只得到文件名和大小。worker 收到的 JSONL `params` 是 `{ input, files }`，其中 `files` 才包含经过宿主规范化的本地路径。每个授权最多 24 个文件、15 分钟后过期、只能由发起插件使用一次；插件停用、更新、卸载或 iframe 生命周期清理也会撤销它。worker 的 stdout/stderr 会回到调用 iframe，因此受信任的二进制仍可主动回显路径；文件授权不是针对二进制的保密或沙箱边界。调用 native worker 等同于执行该插件的本机代码，安装前必须审阅来源与二进制。

`run.timeoutMs` 只适用于用户正在前台等待结果的一次性 worker；宿主会在上限到达时终止**声明的 worker 进程**。它不是后台任务、进度或取消 API；当前版本也不会递归终止该 worker 自行启动的 FFmpeg 等子进程。需要子进程的 worker 必须自行建立、停止和回收它们，且插件不应把“关闭 iframe/停用插件”当作取消信号。`run.timeoutMs` 会进入来源锁和例行更新的安全比较，更新不能静默把短命令改成长时间任务。

### 浏览器屏幕选择器与焦点租约

若插件在用户点击后调用浏览器的 `navigator.mediaDevices.getDisplayMedia()`，必须声明 `screenCapture: true`；调用 `getUserMedia({ audio: true })` 则必须另外声明 `microphone: true`。Rust 只有在对应清单权限、可见 `Surface` purpose 和当前活动 lease 都匹配时，才让可信 React host 为这个跨 origin iframe 添加对应 Permissions Policy 委派；隐藏搜索 runtime、未声明插件、过期 lease 与浏览器安全预览都没有这些委派。在**同一个用户手势处理流程中**还可申请临时焦点租约，防止系统屏幕选择器短暂夺走焦点时 iHub 自动隐藏。

委派和焦点租约都不是录屏授权，也不会给插件额外屏幕内容或原生捕获 API：浏览器仍会显示并管理系统选择器，操作系统仍可要求自己的权限（例如 macOS Screen Recording）。浏览器 QA 最多验证 host 属性条件和安全文案，不能证明系统权限、选择器或实际媒体帧在桌面端可用。

```json
{
  "permissions": {
    "screenCapture": true
  }
}
```

```ts
// 先开始异步申请，但绝不能在 getDisplayMedia 前 await 它：
// iframe/Tauri 往返会让 Chromium 丢失这次点击的 transient activation。
const focusLease = ihub.screenCapture.acquireFocusLease().catch((error) => {
  console.warn("iHub focus protection was unavailable", error);
  return null;
});

// 必须紧随用户点击同步调用；这里没有 await。
const streamPromise = navigator.mediaDevices.getDisplayMedia({ video: true, audio: false });
try {
  const stream = await streamPromise;
  // 仅在用户已完成浏览器选择后使用 stream。
} finally {
  const lease = await focusLease;
  if (lease) {
    await ihub.screenCapture.releaseFocusLease(lease.leaseId).catch(() => undefined);
  }
}
```

每个插件同一时间最多保留一个租约；新申请会替换该插件旧租约。宿主全局最多保留 4 个，固定在 90 秒后过期，并会在插件 iframe 销毁、停用、更新、解除链接或卸载时撤销。`releaseFocusLease()` 只能释放本插件持有的随机不透明 ID；其他插件的 ID 会被拒绝，未知或已经过期的 ID 仅返回 `{ released: false }`，便于 `finally` 安全重试。获取租约失败不应阻断浏览器选择器，也不能造成未处理的 Promise rejection。不要把租约 ID 写入设置、日志、剪贴板或网络。

### 浏览器麦克风权限

麦克风与屏幕捕获是两项独立声明。插件必须在 `permissions` 中写入严格布尔值 `"microphone": true`，Rust 才会把这项能力投影到当前可见 `Surface` lease，可信 React host 才会为对应 iframe 添加 `allow="microphone"`。未声明插件、隐藏搜索 `Runtime`、过期 lease 和浏览器安全预览都不带这项委派；renderer 或插件消息不能自报、补充或升级它。声明不会隐式授予 `screenCapture`，也不会绕过 Chromium／操作系统的麦克风授权提示或提供原生录音 API。

```json
{
  "permissions": {
    "microphone": true
  }
}
```

插件应只在清晰的用户动作后调用浏览器媒体 API，并正常处理拒绝或设备缺失。浏览器测试可以验证 iframe 的条件属性，但不能证明桌面端系统授权或实际音频采集成功。

### 原生单像素取色

若插件需要取鼠标下的一个颜色值，请在 `plugin.json` 明确声明 `cursorColor: true`，并且只从用户点击的**可见插件页面**调用：

```ts
const color = await ihub.cursorColor.sampleOnce();
// color is exactly { hex: "#RRGGBB", rgb: "rgb(r, g, b)" }
```

调用并不会直接读取屏幕：iHub 的父层会先显示自己的确认层，用户确认后才签发一次性授权；宿主随后固定等待两秒，原生层只采样光标下一个像素并删除授权。SDK 为这一笔前台请求保留最多两分钟的响应通道，以便 macOS 首次“屏幕录制”授权完成；这不改变宿主的一次性、不可后台化限制。插件不能提交延迟、坐标、区域、显示器、窗口 ID 或“已确认”标记，返回值也绝不会带位置数据。该功能在 Windows/macOS 上可用；macOS 需要用户为 iHub 授予系统“屏幕录制”权限。后台搜索 runtime、过期页面、重复授权或定时轮询都会被拒绝。

### 生命周期与 IPC

所有前端到宿主的调用都归一为：

```ts
{
  pluginId: "ihub-plugin-my-feature",
  method: "commands.register",
  params: { /* JSON only */ }
}
```

在默认 iframe 桥中，SDK 将该对象通过 `ihub-plugin-bridge/v1` 的 `postMessage` 请求发送给父窗口。父窗口验证消息来自当前 iframe **以及该 iframe 租约的精确 loopback origin**、丢弃插件自报的 `pluginId`，使用当前活动插件 ID 并附加**宿主拥有的租约 ID**重建请求后，才调用 Rust 的 `plugin_host_call({ request })`；结果通过 `ihub-host-bridge/v1` 的同一请求 ID 和精确 target origin 回到 iframe。插件消息本身不携带也不能选择该租约；更新、停用、重新链接、解除链接或卸载后，旧租约会被 Rust 侧拒绝。插件作者不需要、也不应依赖直接调用 Tauri `invoke`；原生 worker 仍不是安全隔离，仍只应载入可信代码。

SDK 为宿主经父窗口桥分派的以下事件提供监听接口：

| 事件 | 载荷 | 插件响应 |
| --- | --- | --- |
| `ihub://plugin/<id>/command` | `requestId`、`commandId`、`input`、`context` | `commands.complete` |
| `ihub://plugin/<id>/search` | `requestId`、`providerId`、`query`、`limit`、`context` | `search.complete` |
| `ihub://plugin/<id>/event/<name>` | 插件定义的 JSON 载荷 | 无；监听器自行处理。 |

这些事件同样应由宿主转换为父窗口到 iframe 的 `postMessage`；SDK 不支持插件直接订阅 Tauri 事件。`bootstrapPlugin` 成功激活后发送 `lifecycle.ready`，销毁时发送 `lifecycle.dispose`。命令和搜索处理器必须尽快返回；慢任务应首先给出可用结果，再通过插件自己的 UI 或事件继续展示进度。


## 原生二进制后端

需要 OCR 引擎、FFmpeg、系统 API 或现有 CLI 时，在 `plugin.json` 里声明二进制：

```json
"backend": {
  "protocol": "jsonl-rpc-v1",
  "binaries": [
    { "target": "windows-x86_64", "path": "bin/windows-x86_64/worker.exe" },
    { "target": "darwin-aarch64", "path": "bin/darwin-aarch64/worker" }
  ]
}
```

宿主只启动当前平台匹配的二进制，并通过 stdin/stdout 使用一行一个 JSON-RPC 2.0 消息的 `jsonl-rpc-v1` 协议：

```json
{"jsonrpc":"2.0","id":"42","method":"ocr.recognize","params":{"input":{"language":"zh-Hans"},"files":[{"path":"<host-granted-path>","name":"image.png","size":12345}]}}
{"jsonrpc":"2.0","id":"42","result":{"text":"Recognized text"}}
```

协议规则：

- stdout 只能写 JSON-RPC 行；日志、进度诊断和第三方库输出必须走 stderr。
- 每一行是完整 UTF-8 JSON，最大请求/响应大小由宿主限制；二进制数据应写临时文件并传路径，而不是塞进 JSON。
- 使用完全相同的 `id` 回应请求；当前宿主要求 stdout 恰好一条非空响应行，错误遵循 JSON-RPC `error.code`、`message`、`data` 结构。
- 当前 MVP 为二进制命令提供 `IHUB_PLUGIN_ID`、`IHUB_COMMAND_ID` 环境变量，并以 JSONL 写入标准输入；请求数据不会再镜像进环境变量。生产协议可再补充版本、数据目录与协议标识；不要假定工作目录是插件目录。
- 二进制须自行处理取消、超时和子进程回收。当前宿主为每次调用启动并等待一个 worker；省略 `run.timeoutMs` 时最多等待 60 秒，显式声明的前台上限最多 30 分钟。到期时宿主仅终止并回收声明的 worker，不承诺杀死它启动的进程树。`backend.restart` 不是已实现的常驻/自动重启语义，插件不能依赖它。

原生后端可以由 Rust、Go、C/C++、Python 打包程序或现有 CLI 实现。iHub 会为每次 Git 导入写入来源、请求 ref、实际解析 commit 和安装时间的 source lock；新导入还会锁定 `plugin.json`、整个可服务的前端构建目录、清单声明图标，以及每个原生二进制的 SHA-256。后续打开前端、读取图标、执行命令或检查/应用更新时若这些文件与锁不一致，iHub 会拒绝加载或运行该快照；重新导入才能生成新的经用户确认的锁。例行 Git 更新会拒绝任何桥接权限或原生二进制声明变化；发布者签名与界面中的逐字段权限/哈希 diff 仍是下一阶段能力。

## GitHub 直装与去中心化分发

### 当前 MVP

iHub 的插件中心只是一个可选发现源，不是唯一下载源。当前 MVP 的 GitHub 导入器接受以下三种写法（均可附 `@tag-or-branch`，URL 可附 `#ref`）：

- GitHub 仓库 URL，例如 `https://github.com/acme/ihub-plugin-weather`；
- `github:owner/repo`，例如 `github:acme/ihub-plugin-weather`；
- 裸 `owner/repo`，例如 `acme/ihub-plugin-weather`。

它会先解析远端 ref 的实际 commit，再检出并复核这个 commit，读取包根（或单层子目录）的 `plugin.json` / `ihub.plugin.json`，最后将来源、请求 ref、实际 commit 与安装时间写入本机 source lock。导入过程中只运行 Git 并读取已构建的插件文件：**不会**执行 `npm install`、`pnpm install`、构建脚本、Git hook 或仓库中的任意 package script。

这不代表 Git 仓库本身安全：插件的前端和经用户选择启动的原生二进制仍是不受沙箱限制的代码。当前 MVP 已提供 ref 选择、source lock、显式本地链接，以及“检查更新 → 用户确认 → 暂存校验后原子替换”的 Git 快照刷新；检查阶段只重新解析保存的来源/ref，不写入 lock、不检出代码也不启动插件。新 Git 快照会为 manifest、完整前端资产目录、声明图标和 manifest 声明的原生二进制写入 SHA-256，并在加载/执行/更新前复核；自动探测会先验证这些记录，不完整或不匹配就跳过并保留手动检查入口。普通例行更新若发现桥接权限、原生二进制、原生命令参数、执行声明或 `run.timeoutMs` 变化会拒绝替换，要求用户卸载、审阅后走显式导入；界面尚不提供逐字段权限/哈希 diff，也不提供第三方二进制的静默自动更新。对同一来源重新导入会解析并锁定当时选择的 ref；它不是替代受审阅 Release 的完整供应链验证。仅开发者可通过插件中心显式链接一个本地绝对目录；该链接不复制源文件，也不会让 Git 导入器接受本地路径。

### 作者从生成模板发布

1. 在项目目录自行运行 `pnpm install`，再运行 `pnpm build`。该命令会生成 `dist/` 并执行静态预检；若随后改动了 `plugin.json` 或加入 worker 工件，再运行一次 `pnpm verify`。
2. 若启用原生 worker，先按生成项目的 `docs/ENABLE_NATIVE_WORKER.md` 构建、测试，并只声明确实存在的 Windows/macOS target。`nativeApi` 只在 TypeScript 前端实际调用 `ihub.native.runCommand()` 时需要。
3. 审阅 `git status`，提交 `plugin.json`、`dist/`、以及每个已声明 `bin/<target>/` 工件；不要依赖未提交目录、`node_modules/` 或导入时自动构建。
4. 发布 tag 或记录完整 commit 后，在插件中心输入 `owner/repo@v1.2.0`（或带 `#ref` 的完整 URL）。不带 ref 时会解析远端 `HEAD`；它仍会锁定这次的 commit，但不适合作为稳定发布引用。

本地链接的作用是让 iHub 直接读取开发目录，不是构建器或文件监视器。每次重建后关闭并重新打开插件前端；链接操作本身从不执行项目脚本、worker 或二进制。

### 生产分发规范（后续实现）

生产导入器应在安装前解析并显示指定的 ref（分支、tag 或 commit）。它应当：读取并 JSON Schema 校验根目录 `plugin.json` → 展示版本、来源、权限、二进制平台及与上次版本的权限变化 → 由用户确认 → 原子复制/检出到插件存储目录 → 写入 lock 文件。

该 lock 文件应至少保存规范化来源、请求的 ref、解析的不可变提交、包与每个平台二进制的 SHA-256、签名身份/验证结果，以及用户确认过的权限集。当前 Git 刷新沿用 lock 中的来源和 ref，并要求用户在每次替换前确认；已实现的自动部分仅是受限的只读发现（官方 stable `autoUpdate: true` 加 immutable lock 和完整 SHA-256 记录），并不静默替换任何代码。未来若支持无交互替换，仍必须重新验证签名与哈希，并在权限新增、GitHub owner 变化、签名失效或哈希不符时再次要求确认。

仓库作者现在就应发布不可变 tag、提供 release notes、让构建产物可复现，并在 GitHub Releases 附上每个平台二进制的 SHA-256；这样才能平滑接入上述生产校验。不要把长期 token、私钥或用户数据写入仓库或前端 bundle。

## 安全与信任（无沙箱）

**iHub 不把含原生 worker 的插件伪装成沙箱。** TypeScript 前端运行在每 iframe 独立的 loopback remote origin，只能通过经 origin 验证的宿主 Bridge 请求能力；但原生二进制仍能以启动 iHub 的用户权限读取、写入、联网、启动进程或调用系统 API。即使清单没有请求某项权限，恶意二进制仍可能绕过桥接层直接使用操作系统能力。

因此 `permissions` 的作用是三件事：安装前让用户看见风险、让宿主拒绝未声明的桥接调用、让更新流程识别权限提升；它不是对受信任二进制的安全边界。当前宿主已把敏感的**前端桥接调用**作为权限 gate，未声明的 clipboard、shell、notification、用户选择目录或 iHub 主启动器布局调用会被拒绝；例行更新也会拒绝任何权限、网络目标、全局快捷键、native API 或原生二进制声明变化。原生二进制本身仍不受这个 gate 约束，且完整的安装确认与字段级更新差异界面仍属于后续生产分发流程。`process.spawn` 尚未实现为宿主 Bridge API；需要启动本机程序的插件应使用其经过清单锁定的原生 worker。实现与审核必须遵守：

- 默认拒绝高风险桥接调用，按最小权限声明；原生 worker 的声明、哈希与启动参数都应进入可审计发布物。
- 生产安装、更新和 GitHub 直装必须展示作者、仓库、固定提交/版本、二进制哈希、目标平台和完整权限清单。
- 官方插件应使用受保护发布分支、可审计 CI、签名 release 与可复现构建。第三方插件应被标为“社区来源”。
- 用户不得安装来源不明、仅通过私信发送、或权限与功能描述不相称的插件；有疑虑时使用专门的低权限系统账户或虚拟机测试。
- 插件作者不得收集、上传或记录用户内容，除非功能明确需要且已在隐私说明、权限和交互界面中说明并取得同意。

无沙箱是为了让专业插件具备真正的桌面能力，而不是降低安全标准。来源可追溯、哈希锁定、权限差异确认和快速撤销是这一设计的必需组成部分。

## 发布前检查

- `pnpm build` 成功，`entry.frontend` 指向实际输出。
- `plugin.json` 同时通过 `manifest.schema.json` 和 SDK 的 `validateManifest`。
- Windows 与 macOS 的每个已声明 target 都有可执行文件并完成真机冒烟测试。
- 二进制 stdout 没有非 JSON-RPC 日志；崩溃、取消、网络离线和超时有明确行为。
- 最小化权限，并在 changelog 中写出新增权限和数据流向。
- 为生产分发准备发布 tag、提交 SHA 与包／二进制 SHA-256；iHub 现有 source/integrity lock 会在导入时锁定实际 commit 并复核产物，不要以可变分支或发布者签名尚未存在为理由弱化审阅。
