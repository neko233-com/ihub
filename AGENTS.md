# iHub 项目执行规范

## 界面与产品基线

- Apple 高饱和系统色和材质层级是 iHub 唯一的配色基线：使用蓝 `#0A84FF`、靛 `#5E5CE6`、紫 `#BF5AF2`、粉 `#FF375F`、橙 `#FF9F0A`、薄荷绿 `#30D158`、青 `#64D2FF` 作为明确的功能与状态色。禁止把冷灰、纯灰或其他产品的灰白色板作为主视觉；中性色只可用于文字、分隔与无状态背景。
- uTools、dtools、Spotlight 或任何外部产品只能参考信息架构、操作路径、尺寸密度与可达性，绝不参考或复制其配色方案；后续改动必须继续沿用 Apple 高饱和 token 和半透明玻璃材质层级。
- UI 必须按用户给出的参考图逐个 surface 实测复刻布局、密度与层级；不得用“全局浅色”或“全局深色”覆盖不同 surface 的真实样式。启动器与管理面使用明亮玻璃层叠；本地搜索工作台可使用深色，但必须是饱和深蓝/靛蓝基底，不能是中性深灰。
- 本地搜索工作台以用户提供的三栏参考图为布局验收基线：顶部搜索范围与输入、左侧类型过滤、中部稠密结果、右侧文件详情预览、底部排序/预览/计数必须在同一 surface 中完成。
- 文件、文件夹和应用在 Windows 桌面端必须优先显示宿主从当前索引目标解析出的系统原生图标；不得用自制 EXE 图标冒充原生图标。浏览器预览或宿主暂未返回图标时才使用中性占位。
- 去除产品中的所有广告内容与广告位，包括横幅、弹窗、插屏、推广卡片、赞助展示、第三方广告脚本及其相关占位元素。新增或修改功能时不得引入广告。

## Windows 验收与本机安装

- Windows 10/11 x64 是当前运行验收平台；macOS 只保留架构与编译扩展点，不得在没有真机验证时宣称已完成运行支持。
- 不得手工复制 `target/release` 下的 EXE 到安装目录，也不得使用 `Stop-Process`、`taskkill`、`Stop-ScheduledTask` 或其他强杀方式替换正在运行的 iHub。
- 本地安装只允许使用 `scripts/dev.ps1 -InstallLatest` 或 `scripts/install-dev.ps1 -NoLaunch -InstallLatest`。Git 安全更新与本地安装必须分开执行；不得通过 `reset`、`checkout`、`clean`、`stash` 或强制合并追求“最新”。
- 持久开发任务只能属于当前用户并使用 `Interactive`、`Limited` 和隐藏的 PowerShell `-File` wrapper；禁止 `SYSTEM`、提权、保存密码、`-Command` 或直接把 `ihub.exe` 作为计划任务动作。
- Windows 后台 Rust 子进程必须使用 `background_command`/`CREATE_NO_WINDOW`；Node 子进程必须使用 `windowsHide: true`。从 Explorer 或任务计划启动时，第一条进程创建指令就必须隐藏窗口。
- Windows 常驻 iHub 必须始终注册可见的系统托盘图标与“显示 iHub”入口；`TrayIconBuilder` 必须显式绑定已打包的默认应用图标，不能只留下无界面的后台进程。
- Tauri updater 的 `.sig` 不等于 Authenticode。若本地安装器是 `NotSigned`，只能说明 updater sidecar 与 payload proof 已验证，不得声称具有 Windows 发布者签名。
- 只有 `launcherMarker=trusted`、两个持久任务均 owned 且 Running、`watcherService.state=healthy`、`lastError=null`，并且安装 EXE 的 SHA-256 同时等于 watcher fingerprint 与安装 proof，才能宣称“本机已安装当前最新版”。

## 交付纪律

- 修改时维护 `.gitignore`，不得提交构建目录、安装产物、运行状态、缓存、临时截图或本机凭据。
- 按功能阶段提交；所有功能、测试、浏览器验证、本机安装与桌面验证全部通过后才允许最终 `git push`。
- 不得用递增版本号、创建 tag 或发布 Release 代替完成功能与验收。版本号只能在功能、测试、本机打包安装和桌面验收全部通过后变更一次；同一候选版失败时继续修复，不得靠连续 bump 版本号重试。
- 最低验证包含 `pnpm check`、`pnpm test`、相关 Rust fmt/check/clippy/test，以及 `scripts/verify-windows-development-scripts.ps1`。
- 已安装 iHub 正在运行时必须提示并等待它正常退出；不能为了完成安装而强制结束进程。

## Windows 发布纪律

- 禁止使用 GitHub Actions 执行 iHub 的 CI、构建、测试、打包、签名、上传或发布；仓库不得保留会触发 Actions 的 workflow。若误触发 Actions，必须立即取消。
- 当前稳定版只发布 Windows 10/11 x64；不得上传 macOS 产物，也不得在没有真机验收时宣称 macOS 已发布。
- 稳定版必须在本机通过 `scripts/publish-windows-release.ps1` 一次性完成验证、Tauri updater 签名、NSIS/MSI 打包、`latest.json`、SHA-256 清单、GitHub 草稿上传、远端回读校验与正式发布。
- 稳定版发布不得跳过验证或复用旧打包产物；发布脚本必须重新执行完整验证并重新生成 NSIS、MSI 和 updater 签名，不提供 `SkipValidation`、`SkipBuild` 或同类逃生参数。
- 发布脚本必须只读取本机密钥文件或进程环境变量，不得打印、复制、提交或上传私钥与密码。Tauri updater `.sig` 仍不等同于 Authenticode。
- 发布 tag 必须等于源码版本且绑定已推送的 `main` 精确提交；不得移动或覆盖已存在的 tag，不得公开未经本地与远端双重校验的草稿。
