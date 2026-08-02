# Windows 发布与安装

iHub 当前只把 Windows 10/11 x64 作为 stable 平台。稳定版必须在维护者的 Windows 本机通过 `scripts/publish-windows-release.ps1` 完成；仓库不保留会触发 GitHub Actions 的 workflow，也不使用 Actions 做 CI、构建、签名、上传或发布。macOS 只保留架构与脚本扩展点，在真机签名、公证和验收完成前不发布资产，也不宣称运行支持。

发布脚本每次都会重新执行完整验证、Tauri updater 签名、NSIS/MSI 打包、`latest.json` 和 `SHA256SUMS.txt` 生成、GitHub draft 上传、远端资产下载回读与哈希复核，最后才把 draft 公开为 stable。脚本不提供跳过验证或跳过打包的参数，也不会覆盖已经公开的同名 Release 或移动已有 tag。

## 发布门槛

版本号不是重试计数器。功能、测试、本机打包安装和真实桌面验收全部通过前，不得修改版本号、创建 tag 或发布 Release。候选版失败时继续在当前源码版本上修复；所有阻断项关闭后，只递增一次 `package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` 和 `src-tauri/Cargo.lock` 的版本。

发布前必须满足：

1. uTools / GitHub 插件导入链路和本次变更的关键功能已在 Windows 桌面端真实操作通过。
2. 启动器与管理面拖拽会实际改变原生窗口坐标，短点击仍能聚焦或激活原控件。
3. `pnpm check`、`pnpm test`、Rust fmt/check/clippy/test、官方插件锁和 Windows PowerShell 安全检查通过。
4. 使用 `scripts/dev.ps1 -InstallLatest` 或 `scripts/install-dev.ps1 -NoLaunch -InstallLatest` 重新生成 NSIS 并安装；不得复制 `target/release` 中的 EXE。
5. 已安装 iHub 已由应用内“退出 iHub”正常退出；不得用 `Stop-Process`、`taskkill` 或计划任务强杀。
6. 工作树干净，当前分支为 `main`，`HEAD` 已推送且精确等于 `origin/main`。

## 本机发布

先做不修改远端的配置预检：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/publish-windows-release.ps1 -Tag vX.Y.Z -PlanOnly
```

确认版本提交已经推送到 `main` 后执行正式发布：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/publish-windows-release.ps1 -Tag vX.Y.Z
```

默认从 `%LOCALAPPDATA%\iHub\keys\tauri-updater-release-v2.key` 读取 updater 私钥；若存在 `%LOCALAPPDATA%\iHub\keys\tauri-updater-release-v2.password`，会读取该文件作为密码。也可以显式传入 `-UpdaterPrivateKeyPath` 与 `-UpdaterPasswordPath`。脚本只把路径交给 Tauri，并在进程内短暂设置签名环境变量，不打印、复制、提交或上传私钥和密码。

若只需上传并远端复核 draft，可加 `-DraftOnly`。正式 stable 仍须在本机确认 draft 资产一致后，再用同一个脚本和同一个 tag 完成公开；已公开的同 tag Release 永远不会被重写。

## 用户安装

Windows 10/11 x64 用户可下载最新稳定版并验证 `SHA256SUMS.txt` 后静默安装：

```powershell
$script = Join-Path $env:TEMP 'ihub-install.ps1'
Invoke-WebRequest https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.ps1 -OutFile $script
Unblock-File $script
& $script
```

可用 `-Repository owner/repository`、`-Version vX.Y.Z` 指定 fork 或版本，或用 `-Interactive` 显示安装器界面。`-RequireAuthenticodeSignature` 只在安装器具有受 Windows 信任的发布者签名时通过。

## 完整性与签名边界

- `SHA256SUMS.txt` 保护首次下载安装器，安装脚本在执行前强制验证。
- Tauri updater `.sig` 与客户端内置公钥保护应用内更新；它不等于 Windows Authenticode 发布者签名。
- 当前安装器如果显示 `NotSigned`，只能说明没有 Authenticode。不得把 updater 签名、payload proof 或 SHA-256 清单描述为 Windows 发布者签名。
- 本机开发安装的 payload proof 证明同一次构建产生的 EXE 被精确安装，不替代 Release 签名或清单。
- updater 私钥只能来自本机密钥文件或进程环境变量，绝不能提交、上传或出现在日志中。

公开后必须重新读取 GitHub Release，确认 tag、stable 状态、Windows-only 资产、`latest.json` URL/签名和远端 SHA-256 与本机一致。应用内更新还需用已安装旧版做真实下载、安装和重启验收；仅能打开下载链接不算通过。
