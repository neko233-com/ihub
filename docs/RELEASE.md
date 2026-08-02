# 发布与安装

iHub 当前正式发布由 `.github/workflows/release.yml` 完成，并以已经实机验收的 Windows 10/11 x64 为 stable 平台，产出 NSIS 与 MSI。发布资产使用稳定命名，例如 `ihub_0.1.0_windows_x64_setup.exe`，因此安装脚本不依赖易变的产品显示名。Windows 的 NSIS 安装模式明确为当前用户（目标为 `%LOCALAPPDATA%`）；若系统缺少 WebView2，安装器使用 Microsoft 的下载 bootstrapper。macOS 保留构建与安装脚本扩展点，但在 Developer ID 签名、公证与真机验收完成前不属于 stable 发布承诺。

## 用户安装

`scripts/install.ps1` 和 `scripts/install.sh` 都会从 GitHub Release 获取安装包，并强制要求同一 Release 提供 `SHA256SUMS.txt` 及所选安装包的有效条目；只有 SHA-256 校验成功后才会执行或安装。清单缺失、条目不匹配或校验失败都会停止，通常意味着 Release 不完整、资产被替换或选错了版本。

Windows 10/11 x64（包括支持 x64 仿真的 Windows on ARM）可在 PowerShell 中运行：

```powershell
$script = Join-Path $env:TEMP 'ihub-install.ps1'
Invoke-WebRequest https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.ps1 -OutFile $script
Unblock-File $script
& $script
```

默认使用最新稳定版；可改为指定版本、私有 fork，或要求 Authenticode 必须有效：

```powershell
& $script -Repository owner/ihub-fork -Version v0.1.0 -RequireAuthenticodeSignature
```

也可通过 `IHUB_REPOSITORY`、`IHUB_VERSION` 环境变量提供相同默认值；显式参数优先。默认是 NSIS 的静默、当前用户安装。添加 `-Interactive` 可显示安装器界面。首次运行可能会由 Tauri 安装或更新 Microsoft Edge WebView2，因此没有该运行时的设备需要联网完成首次安装。

macOS 安装脚本作为后续发布扩展点保留；当前 stable Release 不上传 DMG，因此暂不应对终端用户执行下面的命令：

```bash
curl -fsSL https://raw.githubusercontent.com/neko233-com/ihub/main/scripts/install.sh -o /tmp/ihub-install.sh
bash /tmp/ihub-install.sh
```

脚本默认安装到 `/Applications/iHub.app`，必要时会要求管理员密码。它会选择当前 CPU 对应的发行资产，并在复制前确认 DMG 恰好包含一个常规 `iHub.app`，其 `ihub` 可执行文件匹配所选架构。若只希望为当前用户安装，或只接受已通过 Apple 签名和 Gatekeeper 检查的构建：

```bash
bash /tmp/ihub-install.sh --application-dir "$HOME/Applications" --require-signature
```

也可以使用 `--repository owner/repository`、`--version v0.1.0`，或用 `IHUB_REPOSITORY`、`IHUB_VERSION` 环境变量提供对应默认值；显式参数优先。安装成功后默认打开 iHub，加入 `--no-launch` 可只安装而不启动。引导脚本本身通过 HTTPS 从 GitHub 获取；在高保证环境中，请先固定并审阅一个 Git tag 后再执行脚本，而不要直接把网络内容管道给 shell。

## 签名与完整性模型

SHA-256 清单保护首次安装器。它由工作流在 Windows 资产上传后生成，并与同一 GitHub Release 一起发布。在公开 draft 之前，工作流还会验证 `latest.json`：当前承诺平台 `windows-x86_64` 必须有非空签名，并且 URL 必须是当前仓库、当前 tag 已上传 updater 资产的规范 GitHub HTTPS `browser_download_url`；包含 userinfo、端口、query、fragment 或编码歧义的 URL 会被拒绝。缺少安装器、签名或平台条目都会保留 draft。验证器仍支持显式检查 macOS 平台，以便未来在完成签名、公证与真机验收后恢复跨平台矩阵。

Tauri 的应用内自动更新是另一条链路：`TAURI_SIGNING_PRIVATE_KEY` 对 updater 资产签名，客户端内置的公开密钥验证 `latest.json` 中的签名。`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 只在私钥加密时需要。私钥绝不能提交、上传到 Release 或复用为 macOS/Windows 的代码签名证书；丢失该私钥会使既有客户端无法信任后续更新。

Windows 的本机开发安装还有一条独立的“本次构建确实落盘”证明：Tauri 交给 makensis 的 `NSS` 主程序会以原文件名复制为不可变输入，安装后再以 SHA-256、长度和随机 nonce 对照精确目标。它解决的是本地同版本重装与打包后 marker 恢复造成的误判，不替代 GitHub Release 的 updater 签名、`SHA256SUMS.txt` 或 Authenticode 信任链。

macOS 的 Developer ID 签名和公证、Windows 的 Authenticode 签名与 Tauri updater 签名不同：

- `install.sh --require-signature` 同时要求 `codesign` 和 Gatekeeper (`spctl`) 成功；未加该参数时，SHA-256 仍是强制项，但脚本会提示签名状态。
- `install.ps1 -RequireAuthenticodeSignature` 要求 Windows 信任链中的 Authenticode 签名有效；未加该参数时，脚本会提示状态而不会把未签名状态当作校验成功。
- 恢复 macOS 正式发布前，必须重新接入 Apple Developer ID 签名和 notarization 所需 secrets，并在真机上完成安装与更新验收；当前 Windows stable 工作流不会生成或宣称 macOS 资产。
- Windows Authenticode/Trusted Signing 仍需由组织选定提供商并接入后才能宣称 SmartScreen 友好。当前工作流不会把 updater 私钥误当作代码签名证书，`install.ps1 -RequireAuthenticodeSignature` 也会在未接入该步骤时明确失败。

## 首次发布前的维护者清单

1. 保持 `package.json` 与 `src-tauri/tauri.conf.json` 的版本相同，并提交 `pnpm-lock.yaml`。工作流使用 `pnpm install --frozen-lockfile`。
2. 本地生成 updater 密钥，例如 `pnpm tauri signer generate -w ./ihub-updater.key`。将公开密钥填入 Tauri 配置的 updater `pubkey`，将私钥内容作为 GitHub Secret `TAURI_SIGNING_PRIVATE_KEY`；只有私钥加密时才另设 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。不要提交 `ihub-updater.key`。
3. 若要恢复 macOS 发布，先配置 Apple Secrets：`APPLE_CERTIFICATE`（base64 编码的 Developer ID `.p12`）、`APPLE_CERTIFICATE_PASSWORD`、`APPLE_SIGNING_IDENTITY`、`APPLE_ID`、`APPLE_PASSWORD`、`APPLE_TEAM_ID` 和随机的 `KEYCHAIN_PASSWORD`，恢复 macOS 构建矩阵，并完成真机验收；在此之前保持 Windows-only stable。
4. 为 Windows 增加所选证书服务的 post-build 签名步骤，并用 `-RequireAuthenticodeSignature` 做一次干净机器验收。证书、令牌和私钥都只能进入 GitHub Secrets 或受管密钥服务。
5. 运行 `pnpm verify:official-plugins`（本地独立插件仓库）以及 `node scripts/verify-official-plugin-lock.mjs --remote`（远端不可变 tag），再运行 `powershell -ExecutionPolicy Bypass -File scripts/validate-github-actions.ps1`、`node scripts/verify-release-assets.mjs --help` 和项目的常规检查。

当以上条件就绪后，创建匹配版本的 tag：

```bash
git tag v0.1.0
git push origin v0.1.0
```

工作流先把请求的 tag 解析或创建到当前精确提交，复用既有 draft 时也重新验证 tag → commit 绑定；随后输出固定的 `release_sha`，Windows 构建检出这个 SHA。它创建或复用同一个**draft** Release，上传安装器、Tauri updater 签名和当前 `latest.json` 后读取这个精确 draft 的 Release 元数据，把清单中的资产 API URL 严格映射为同一 Release 返回的 `browser_download_url`，再覆盖上传规范化后的清单。Windows 资产完成后才下载完整资产集，验证安装器、签名及 URL 关联，生成强制的 `SHA256SUMS.txt`；公开前再次复核精确 draft ID、tag → `release_sha` 与 draft 状态，并按 release ID 发布。任何 commit／release 身份绑定、构建、签名预检、规范化或最终清单验证失败都会保留 draft；已公开的同 tag Release 不会被重写。手动触发工作流时也必须提供完全匹配 `package.json` 版本的 tag。

## 自动更新验收

在发布前，以已安装的 Windows 旧版本验证：应用能取得 `releases/latest/download/latest.json`、平台键为 `windows-x86_64`、签名可验证、并能安装更新。不要仅靠下载链接可访问作为通过标准；工作流的 JSON/资产关联检查不能替代真实机器上的下载、安装与重启验收。未来恢复 macOS 发布时，必须分别补做 `darwin-aarch64` 与 `darwin-x86_64` 真机验收。

若需要轮换 updater 密钥，必须先发布能同时信任旧/新密钥的迁移版本；直接替换公开密钥会令已安装客户端拒绝之后的更新。
