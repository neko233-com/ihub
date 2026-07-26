# iHub 插件开发指南（v1）

iHub 插件是一个可发布的目录：前端使用 TypeScript（推荐 Vite），可选一个或多个 Windows/macOS 原生二进制后端。前端通过 `@ihub/plugin-sdk` 调用宿主能力；原生后端通过标准输入/输出的 JSON Lines RPC 工作。

这套机制刻意支持 OCR、FFmpeg、系统自动化等原生能力。它**不是沙箱机制**：安装包含二进制的插件等同于运行本机代码。请先阅读本文的[安全与信任](#安全与信任无沙箱)章节。

## 五分钟开始

先在 **iHub 主仓库的 `examples/` 下**复制模板。模板中的 `file:../../plugin-sdk` 依赖和 Vite alias 都故意指回主仓 SDK；复制到这个位置可以直接工作。

```powershell
Set-Location <iHub 主仓库根目录>
Copy-Item -Recurse examples/ihub-plugin-hello examples/ihub-plugin-my-feature
Set-Location examples/ihub-plugin-my-feature
pnpm install
pnpm dev
```

模板是纯 TypeScript + Vite，没有 React 运行时依赖。浏览器中运行 `pnpm dev` 时，模板显式把 `createDevelopmentBridge()` 传给 SDK。

当前 MVP **没有**“加载本地插件目录”或热重载入口；浏览器预览也不等于在 iHub 中运行。若要验证当前宿主桥，请先构建插件、提交到可访问的 Git 仓库，再通过下文的 [GitHub 直装](#github-直装与去中心化分发) 导入该构建产物。导入的是一个 Git 快照，不会监听本地文件变更。

### 将模板放入独立仓库

独立仓库不能继续使用模板里的相对 `file:../../plugin-sdk` 依赖和 `vite.config.ts` 中指向 `../../plugin-sdk/src/index.ts` 的 alias。开发期可使用全局链接：

1. 在独立插件仓库中，先从 `package.json` 删除 `@ihub/plugin-sdk` 的 `file:../../plugin-sdk` 依赖，并删除 `vite.config.ts` 的 `resolve.alias["@ihub/plugin-sdk"]`；这样 Vite 会从 `node_modules` 解析已构建的 SDK。
2. 在 iHub 主仓库根目录构建并注册 SDK：

   ```powershell
   pnpm --dir plugin-sdk build
   pnpm --dir plugin-sdk link --global
   ```

3. 回到独立插件仓库，安装其余开发依赖，再连接全局 SDK：

   ```powershell
   pnpm install
   pnpm link --global @ihub/plugin-sdk
   pnpm dev
   ```

全局链接只适合本地开发，不会写成可复现的发布依赖；每次会重建 `node_modules` 的 `pnpm install` 后，都应重新执行最后一条 `pnpm link --global @ihub/plugin-sdk`。SDK 发布到 npm 后，应改为在 `package.json` 中声明发布版本（例如 `"@ihub/plugin-sdk": "^1.0.0"`），移除全局链接，并重新执行 `pnpm install`。

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
| `entry.frontend` | 是 | 包内前端入口，例如 `dist/index.html`。 |
| `contributes` | 否 | 命令、搜索提供器、设置和快捷动作的静态声明。 |
| `activationEvents` | 否 | `onStartup`、`onSearch`、`onCommand:<id>` 或 `onFile:<ext>`。 |
| `permissions` | 是 | 前端 Bridge 的能力请求；空对象也必须明确写出。完整安装确认将在生产分发流程中加入。 |
| `backend` | 否 | 与前端配套的原生二进制及其平台目标。 |
| `update` | 否 | 声明稳定/测试通道和自动更新偏好；当前 MVP 不执行插件自动更新。 |

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
    "commands": [{ "id": "open-my-feature", "title": "Open My feature" }]
  },
  "permissions": {
    "notifications": true
  }
}
```

清单中的包内路径不能是绝对路径，也不能借由 `..` 离开插件根目录。每个 `backend.binaries[].target` 只能声明一次：`windows-x86_64`、`windows-aarch64`、`darwin-x86_64`、`darwin-aarch64`。

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
      actions: [{ id: "open", title: "Open" }]
    }],
  );
});
```

`PluginContext` 提供的宿主 API：

| API | 对应权限 | 说明 |
| --- | --- | --- |
| `commands.register` | 无 | 注册命令处理器。 |
| `search.register` | 无 | 注册低延迟搜索提供器。 |
| `settings.get/set` | 无 | 插件命名空间下的设置存储。密钥设置应在清单上标注 `secret`。 |
| `clipboard.readText/writeText` | `clipboard.read/write` | 读写剪贴板文本。 |
| `notifications.show` | `notifications` | 显示本机通知。 |
| `shell.openExternal/openPath` | `shell.*` | 打开 URL 或文件路径。 |
| `process.spawn` | `process.spawn` | 启动批准范围内的外部进程。优先使用专用 `backend`。 |
| `events.on` | 取决于事件 | 订阅宿主发送给当前插件的事件。 |
| `logger` | 无 | 写入带插件 ID 的宿主日志。 |

不要直接导入 `@tauri-apps/api`。iHub 会把 `entry.frontend` 作为 `asset:` 资源载入宿主控制的 iframe；SDK 在该 iframe 中通过父窗口 `postMessage` 桥访问宿主。`window.__IHUB_PLUGIN_API__` 可作为其他受控宿主表面的替代注入桥。两种生产路径都不向插件 bundle 暴露 Tauri API；SDK 不提供直接调用 Tauri 的后备路径。浏览器预览或测试必须显式传入 `createDevelopmentBridge()` 或自定义 `HostBridge`。

### 生命周期与 IPC

所有前端到宿主的调用都归一为：

```ts
{
  pluginId: "ihub-plugin-my-feature",
  method: "commands.register",
  params: { /* JSON only */ }
}
```

在默认 iframe 桥中，SDK 将该对象通过 `ihub-plugin-bridge/v1` 的 `postMessage` 请求发送给父窗口。父窗口验证消息来自当前 iframe、丢弃插件自报的 `pluginId`，使用当前活动插件 ID 重建请求后，才调用 Rust 的 `plugin_host_call({ request })`；结果通过 `ihub-host-bridge/v1` 的同一请求 ID 回到 iframe。插件作者不能也不需要直接调用 Tauri `invoke`。

SDK 为宿主经父窗口桥分派的以下事件提供监听接口：

| 事件 | 载荷 | 插件响应 |
| --- | --- | --- |
| `ihub://plugin/<id>/command` | `requestId`、`commandId`、`input`、`context` | `commands.complete` |
| `ihub://plugin/<id>/search` | `requestId`、`providerId`、`query`、`limit`、`context` | `search.complete` |
| `ihub://plugin/<id>/event/<name>` | 插件定义的 JSON 载荷 | 无；监听器自行处理。 |

这些事件同样必须由宿主转换为父窗口到 iframe 的 `postMessage`；插件不能直接订阅 Tauri 事件。`bootstrapPlugin` 成功激活后发送 `lifecycle.ready`，销毁时发送 `lifecycle.dispose`。命令和搜索处理器必须尽快返回；慢任务应首先给出可用结果，再通过插件自己的 UI 或事件继续展示进度。


## 原生二进制后端

需要 OCR 引擎、FFmpeg、系统 API 或现有 CLI 时，在 `plugin.json` 里声明二进制：

```json
"backend": {
  "protocol": "jsonl-rpc-v1",
  "restart": "on-failure",
  "binaries": [
    { "target": "windows-x86_64", "path": "bin/windows-x86_64/worker.exe" },
    { "target": "darwin-aarch64", "path": "bin/darwin-aarch64/worker" }
  ]
}
```

宿主只启动当前平台匹配的二进制，并通过 stdin/stdout 使用一行一个 JSON-RPC 2.0 消息的 `jsonl-rpc-v1` 协议：

```json
{"jsonrpc":"2.0","id":"42","method":"ocr.recognize","params":{"path":"C:\\image.png"}}
{"jsonrpc":"2.0","id":"42","result":{"text":"Recognized text"}}
```

协议规则：

- stdout 只能写 JSON-RPC 行；日志、进度诊断和第三方库输出必须走 stderr。
- 每一行是完整 UTF-8 JSON，最大请求/响应大小由宿主限制；二进制数据应写临时文件并传路径，而不是塞进 JSON。
- 使用 `id` 回应请求；通知不带 `id`；错误遵循 JSON-RPC `error.code`、`message`、`data` 结构。
- 当前 MVP 为二进制命令提供 `IHUB_PLUGIN_ID`、`IHUB_COMMAND_ID` 和 JSON 字符串 `IHUB_PLUGIN_INPUT` 环境变量，并以 JSONL 写入标准输入。生产协议可再补充版本、数据目录与协议标识；不要假定工作目录是插件目录。
- 二进制须自行处理取消、超时和子进程回收。`restart: "always"` 只适合无状态常驻服务。

原生后端可以由 Rust、Go、C/C++、Python 打包程序或现有 CLI 实现。生产分发阶段应为每个二进制计算哈希，并在安装锁文件中记录实际解析的提交与完整性信息；当前 MVP 尚未自动验证这些哈希或维护完整的安装锁。

## GitHub 直装与去中心化分发

### 当前 MVP

iHub 的插件中心只是一个可选发现源，不是唯一下载源。当前 MVP 的 GitHub 导入器接受以下三种写法：

- GitHub 仓库 URL，例如 `https://github.com/acme/ihub-plugin-weather`；
- `github:owner/repo`，例如 `github:acme/ihub-plugin-weather`；
- 裸 `owner/repo`，例如 `acme/ihub-plugin-weather`。

它会浅克隆该仓库当前可解析的 `HEAD`，读取包根（或单层子目录）的 `plugin.json` / `ihub.plugin.json`，并将解析出的 `HEAD` 提交记录为这次已安装快照。导入过程中只运行 Git 并读取已构建的插件文件：**不会**执行 `npm install`、`pnpm install`、构建脚本、Git hook 或仓库中的任意 package script。

这不代表 Git 仓库本身安全：插件的前端和经用户选择启动的原生二进制仍是不受沙箱限制的代码。当前 MVP 不提供 ref 选择、本地目录导入、完整性校验、签名校验、可审计 lock 文件或插件自动更新。对同一来源重新导入会再次解析当时的 `HEAD`；它不是不可变版本管理。

### 生产分发规范（后续实现）

生产导入器应在安装前解析并显示指定的 ref（分支、tag 或 commit），并可在明确的开发者模式下导入本地目录。它应当：读取并 JSON Schema 校验根目录 `plugin.json` → 展示版本、来源、权限、二进制平台及与上次版本的权限变化 → 由用户确认 → 原子复制/检出到插件存储目录 → 写入 lock 文件。

该 lock 文件应至少保存规范化来源、请求的 ref、解析的不可变提交、包与每个平台二进制的 SHA-256、签名身份/验证结果，以及用户确认过的权限集。自动更新也属于后续能力：它必须沿用 lock 中的来源和更新通道，重新验证签名与哈希，并在权限新增、GitHub owner 变化、签名失效或哈希不符时再次要求确认。

仓库作者现在就应发布不可变 tag、提供 release notes、让构建产物可复现，并在 GitHub Releases 附上每个平台二进制的 SHA-256；这样才能平滑接入上述生产校验。不要把长期 token、私钥或用户数据写入仓库或前端 bundle。

## 安全与信任（无沙箱）

**iHub 插件并不隔离插件代码。** TypeScript 前端可调用被宿主桥允许的能力；原生二进制则能以启动 iHub 的用户权限读取、写入、联网、启动进程或调用系统 API。即使清单没有请求某项权限，恶意二进制仍可能绕过桥接层直接使用操作系统能力。

因此 `permissions` 的作用是三件事：安装前让用户看见风险、让宿主拒绝未声明的桥接调用、让更新流程识别权限提升；它不是对受信任二进制的安全边界。当前宿主已把敏感的**前端桥接调用**作为权限 gate，未声明的 clipboard、shell、notification 或 process 调用会被拒绝；原生二进制不受这个 gate 约束。完整的安装确认与更新差异确认属于后续生产分发流程。实现与审核必须遵守：

- 默认拒绝高风险桥接调用，按最小权限声明；生产实现应进一步限制 `process.spawn.allow` 列表。
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
- 为生产分发准备发布 tag、提交 SHA 与包/二进制 SHA-256；待 lock 机制落地后再将它们记录为不可变安装版本，不要以可变分支作为生产锁定版本。
