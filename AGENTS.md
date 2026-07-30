# iHub 项目执行规范

## 界面与产品基线

- UI 必须按用户给出的 uTools/Spotlight 参考图逐个 surface 实测复刻尺寸、密度、层级和颜色 token；不得用“全局浅色”或“全局深色”覆盖不同 surface 的真实样式。当前启动器/管理面的明亮基线与本地搜索工作台的深灰基线可以并存。
- 本地搜索工作台以用户提供的 uTools 三栏参考图为验收基线：顶部搜索范围与输入、左侧类型过滤、中部稠密结果、右侧文件详情预览、底部排序/预览/计数必须在同一 surface 中完成。
- 文件、文件夹和应用在 Windows 桌面端必须优先显示宿主从当前索引目标解析出的系统原生图标；不得用自制 EXE 图标冒充原生图标。浏览器预览或宿主暂未返回图标时才使用中性占位。
- 去除产品中的所有广告内容与广告位，包括横幅、弹窗、插屏、推广卡片、赞助展示、第三方广告脚本及其相关占位元素。新增或修改功能时不得引入广告。

## Windows 验收与本机安装

- Windows 10/11 x64 是当前运行验收平台；macOS 只保留架构与编译扩展点，不得在没有真机验证时宣称已完成运行支持。
- 不得手工复制 `target/release` 下的 EXE 到安装目录，也不得使用 `Stop-Process`、`taskkill`、`Stop-ScheduledTask` 或其他强杀方式替换正在运行的 iHub。
- 本地安装只允许使用 `scripts/dev.ps1 -InstallLatest` 或 `scripts/install-dev.ps1 -NoLaunch -InstallLatest`。Git 安全更新与本地安装必须分开执行；不得通过 `reset`、`checkout`、`clean`、`stash` 或强制合并追求“最新”。
- 持久开发任务只能属于当前用户并使用 `Interactive`、`Limited` 和隐藏的 PowerShell `-File` wrapper；禁止 `SYSTEM`、提权、保存密码、`-Command` 或直接把 `ihub.exe` 作为计划任务动作。
- Windows 后台 Rust 子进程必须使用 `background_command`/`CREATE_NO_WINDOW`；Node 子进程必须使用 `windowsHide: true`。从 Explorer 或任务计划启动时，第一条进程创建指令就必须隐藏窗口。
- Tauri updater 的 `.sig` 不等于 Authenticode。若本地安装器是 `NotSigned`，只能说明 updater sidecar 与 payload proof 已验证，不得声称具有 Windows 发布者签名。
- 只有 `launcherMarker=trusted`、两个持久任务均 owned 且 Running、`watcherService.state=healthy`、`lastError=null`，并且安装 EXE 的 SHA-256 同时等于 watcher fingerprint 与安装 proof，才能宣称“本机已安装当前最新版”。

## 交付纪律

- 修改时维护 `.gitignore`，不得提交构建目录、安装产物、运行状态、缓存、临时截图或本机凭据。
- 按功能阶段提交；所有功能、测试、浏览器验证、本机安装与桌面验证全部通过后才允许最终 `git push`。
- 最低验证包含 `pnpm check`、`pnpm test`、相关 Rust fmt/check/clippy/test，以及 `scripts/verify-windows-development-scripts.ps1`。
- 已安装 iHub 正在运行时必须提示并等待它正常退出；不能为了完成安装而强制结束进程。
