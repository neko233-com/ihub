# 发布与安装

iHub 的正式发布由 `.github/workflows/release.yml` 完成：Windows x64 会产出 NSIS 与 MSI，macOS 会分别产出 Apple Silicon (`aarch64`) 和 Intel (`x64`) DMG。发布资产使用稳定命名，例如 `ihub_0.1.0_windows_x64_setup.exe` 与 `ihub_0.1.0_darwin_aarch64.dmg`，因此安装脚本不依赖易变的产品显示名。

## 用户安装

`scripts/install.ps1` 和 `scripts/install.sh` 都会从 GitHub Release 获取安装包，先下载同一 Release 中的 `SHA256SUMS.txt`，校验 SHA-256 后才执行或安装。不要跳过校验失败；它通常意味着下载不完整、资产被替换，或选错了 Release。

Windows 10/11 x64（包括支持 x64 仿真的 Windows on ARM）可在 PowerShell 中运行：

```powershell
$script = Join-Path $env:TEMP 'ihub-install.ps1'
Invoke-WebRequest https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.ps1 -OutFile $script
Unblock-File $script
& $script
```

默认使用最新稳定版；可改为指定版本、私有 fork，或要求 Authenticode 必须有效：

```powershell
$env:IHUB_REPOSITORY = 'owner/ihub-fork'
& $script -Version v0.1.0 -RequireAuthenticodeSignature
```

默认是 NSIS 的静默、当前用户安装。添加 `-Interactive` 可显示安装器界面。首次运行可能会由 Tauri 安装或更新 Microsoft Edge WebView2。

macOS 12+（Apple Silicon 和 Intel）可运行：

```bash
curl -fsSL https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.sh -o /tmp/ihub-install.sh
bash /tmp/ihub-install.sh
```

脚本默认安装到 `/Applications/iHub.app`，必要时会要求管理员密码。若只希望为当前用户安装，或只接受已通过 Apple 签名和 Gatekeeper 检查的构建：

```bash
bash /tmp/ihub-install.sh --application-dir "$HOME/Applications" --require-signature
```

也可以用 `IHUB_REPOSITORY=owner/repository`、`IHUB_VERSION=v0.1.0` 覆盖默认仓库与版本。引导脚本本身通过 HTTPS 从 GitHub 获取；在高保证环境中，请先固定并审阅一个 Git tag 后再执行脚本，而不要直接把网络内容管道给 shell。

## 签名与完整性模型

SHA-256 清单保护首次安装器。它由工作流在所有平台资产上传后生成，并与同一 GitHub Release 一起发布。

Tauri 的应用内自动更新是另一条链路：`TAURI_SIGNING_PRIVATE_KEY` 对 updater 资产签名，客户端内置的公开密钥验证 `latest.json` 中的签名。私钥绝不能提交、上传到 Release 或复用为 macOS/Windows 的代码签名证书；丢失该私钥会使既有客户端无法信任后续更新。

macOS 的 Developer ID 签名和公证、Windows 的 Authenticode 签名与 Tauri updater 签名不同：

- `install.sh --require-signature` 同时要求 `codesign` 和 Gatekeeper (`spctl`) 成功；未加该参数时，SHA-256 仍是强制项，但脚本会提示签名状态。
- `install.ps1 -RequireAuthenticodeSignature` 要求 Windows 信任链中的 Authenticode 签名有效；未加该参数时，脚本会提示状态而不会把未签名状态当作校验成功。
- 正式公开版应启用 Apple Developer ID + notarization，并在 Windows 构建之后接入组织选择的 Authenticode/Trusted Signing 提供商。当前工作流不会把 updater 私钥误当作代码签名证书。

## 首次发布前的维护者清单

1. 保持 `package.json` 与 `src-tauri/tauri.conf.json` 的版本相同，并提交 `pnpm-lock.yaml`。工作流使用 `pnpm install --frozen-lockfile`。
2. 本地生成 updater 密钥，例如 `pnpm tauri signer generate -w ./ihub-updater.key`。将公开密钥填入 Tauri 配置的 updater `pubkey`，将私钥内容作为 GitHub Secret `TAURI_SIGNING_PRIVATE_KEY`，密码（若有）作为 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。不要提交 `ihub-updater.key`。
3. 配置 Apple Secrets：`APPLE_CERTIFICATE`、`APPLE_CERTIFICATE_PASSWORD`、`APPLE_SIGNING_IDENTITY`、`APPLE_ID`、`APPLE_PASSWORD` 和 `APPLE_TEAM_ID`。未配置时 macOS 构建可产出，但不应作为面向普通用户的稳定发布。
4. 为 Windows 增加所选证书服务的 post-build 签名步骤，并用 `-RequireAuthenticodeSignature` 做一次干净机器验收。证书、令牌和私钥都只能进入 GitHub Secrets 或受管密钥服务。
5. 运行 `powershell -ExecutionPolicy Bypass -File scripts/validate-github-actions.ps1`，再运行项目的常规检查。

当以上条件就绪后，创建匹配版本的 tag：

```bash
git tag v0.1.0
git push origin v0.1.0
```

工作流先创建 draft Release，上传三个目标的安装器、Tauri updater 签名和 `latest.json`，随后生成 `SHA256SUMS.txt` 并发布。任何矩阵构建失败都会保留 draft，避免把不完整的跨平台 Release 暴露为 latest。手动触发工作流时也必须提供完全匹配 `package.json` 版本的 tag。

## 自动更新验收

在发布前，以已安装的旧版本验证：应用能取得 `releases/latest/download/latest.json`、平台键与本机一致（`windows-x86_64`、`darwin-aarch64` 或 `darwin-x86_64`）、签名可验证、并能安装更新。不要仅靠下载链接可访问作为通过标准。

若需要轮换 updater 密钥，必须先发布能同时信任旧/新密钥的迁移版本；直接替换公开密钥会令已安装客户端拒绝之后的更新。
